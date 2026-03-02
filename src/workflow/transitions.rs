//! State transition logic for the agent pipeline.
//!
//! Validates and applies state transitions, including kickback validation.
//! Given a message, produces side-effect messages. This pure design (no I/O,
//! no async) makes the engine trivially testable.

use std::collections::HashMap;

use crate::messages::AppMessage;
use crate::tasks::models::TaskId;
use crate::workflow::agents::AgentKind;

/// The lifecycle phase of a task within the workflow engine.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub enum WorkflowPhase {
    /// No work has been started yet.
    Idle,
    /// An agent is actively working on the task.
    Running,
    /// The workflow is paused, waiting for a human answer.
    AwaitingAnswer {
        /// Zero-based index into the task's `questions` list.
        question_index: usize,
    },
    /// The workflow is paused, waiting for human approval to start the next agent.
    AwaitingApproval {
        /// The agent that will start once the human approves.
        next_agent: AgentKind,
        /// Optional context for `CreateSession` (e.g., kickback reason).
        context: Option<String>,
    },
    /// All agents have completed; the task is awaiting human approval.
    PendingReview,
    /// The human has approved the task; it is complete.
    Completed,
    /// An unrecoverable error occurred.
    Errored {
        /// Description of what went wrong.
        reason: String,
    },
}

/// Per-task state tracked by the workflow engine.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct WorkflowState {
    /// The task this state belongs to.
    pub task_id: TaskId,
    /// The agent currently responsible for this task.
    pub current_agent: AgentKind,
    /// The opencode session ID, if a session is active.
    pub session_id: Option<String>,
    /// The current lifecycle phase.
    pub phase: WorkflowPhase,
}

/// Pure state machine that drives tasks through the 7-agent pipeline.
///
/// The engine holds a map of per-task `WorkflowState` entries and processes
/// `AppMessage` values, mutating state and returning zero or more side-effect
/// messages. No I/O or async is performed; this makes the engine trivially
/// testable.
#[allow(dead_code)]
pub struct WorkflowEngine {
    states: HashMap<TaskId, WorkflowState>,
    /// When `true`, pause and wait for human approval before starting the next agent.
    approval_gate_enabled: bool,
}

#[allow(dead_code)]
impl WorkflowEngine {
    /// Creates a new `WorkflowEngine` with an empty state map.
    ///
    /// # Arguments
    ///
    /// * `approval_gate_enabled` - When `true`, the engine pauses after each agent
    ///   completes and waits for [`AppMessage::HumanApprovedTransition`] before
    ///   starting the next agent. Set to `false` for fully-automatic pipeline execution.
    pub fn new(approval_gate_enabled: bool) -> Self {
        Self {
            states: HashMap::new(),
            approval_gate_enabled,
        }
    }

    /// Enables or disables the human approval gate at runtime.
    pub fn set_approval_gate(&mut self, enabled: bool) {
        self.approval_gate_enabled = enabled;
    }

    /// Clears the active session ID for a task without changing its phase or agent.
    ///
    /// Used after a parse failure to allow a fresh `SessionCreated` event to
    /// register the replacement session without being silently dropped by the
    /// duplicate-session guard in `App::handle_message`.
    pub fn reset_session_id(&mut self, task_id: &TaskId) {
        if let Some(state) = self.states.get_mut(task_id) {
            state.session_id = None;
        }
    }

    /// Returns a reference to the workflow state for the given task, if any.
    pub fn state(&self, task_id: &TaskId) -> Option<&WorkflowState> {
        self.states.get(task_id)
    }

    /// Resumes a previously interrupted task at the specified agent.
    ///
    /// Creates a fresh `WorkflowState` in the `Running` phase at `agent`,
    /// overwriting any existing state (including `Errored`). Returns a
    /// `CreateSession` message to restart the agent.
    ///
    /// # Arguments
    ///
    /// * `task_id` - The task to resume.
    /// * `agent` - The agent at which to resume the pipeline.
    pub fn resume(&mut self, task_id: TaskId, agent: AgentKind) -> Vec<AppMessage> {
        let state = WorkflowState {
            task_id: task_id.clone(),
            current_agent: agent,
            session_id: None,
            phase: WorkflowPhase::Running,
        };
        self.states.insert(task_id.clone(), state);
        vec![AppMessage::CreateSession {
            task_id,
            agent,
            prompt: String::new(),
            context: Some("Task resumed".to_string()),
        }]
    }

    /// Applies a message to the engine, mutating state and returning side effects.
    ///
    /// The returned `Vec<AppMessage>` contains zero or more messages that the
    /// caller must dispatch (e.g., `CreateSession` to start an agent).
    pub fn process(&mut self, msg: AppMessage) -> Vec<AppMessage> {
        match msg {
            AppMessage::StartTask { task_id } => {
                let state = WorkflowState {
                    task_id: task_id.clone(),
                    current_agent: AgentKind::Intake,
                    session_id: None,
                    phase: WorkflowPhase::Running,
                };
                self.states.insert(task_id.clone(), state);
                vec![AppMessage::CreateSession {
                    task_id,
                    agent: AgentKind::Intake,
                    prompt: String::new(),
                    context: None,
                }]
            }

            AppMessage::SessionCreated {
                task_id,
                session_id,
            } => {
                if let Some(state) = self.states.get_mut(&task_id) {
                    state.session_id = Some(session_id);
                }
                vec![]
            }

            AppMessage::SessionCompleted {
                task_id,
                session_id: _,
                response_text: _,
            } => {
                let Some(state) = self.states.get_mut(&task_id) else {
                    return vec![];
                };
                match state.current_agent.next() {
                    Some(next) => {
                        state.session_id = None;
                        if self.approval_gate_enabled {
                            state.phase = WorkflowPhase::AwaitingApproval {
                                next_agent: next,
                                context: None,
                            };
                            vec![]
                        } else {
                            state.current_agent = next;
                            vec![AppMessage::CreateSession {
                                task_id,
                                agent: next,
                                prompt: String::new(),
                                context: None,
                            }]
                        }
                    }
                    None => {
                        state.session_id = None;
                        state.phase = WorkflowPhase::PendingReview;
                        vec![]
                    }
                }
            }

            AppMessage::AgentCompleted {
                task_id,
                agent,
                summary: _,
            } => {
                let Some(state) = self.states.get_mut(&task_id) else {
                    return vec![];
                };
                // Ignore stale or out-of-order completions that don't match
                // the current agent; prevents double-advance if both
                // SessionCompleted and AgentCompleted fire for the same step.
                if agent != state.current_agent {
                    return vec![];
                }
                match agent.next() {
                    Some(next) => {
                        state.session_id = None;
                        if self.approval_gate_enabled {
                            state.phase = WorkflowPhase::AwaitingApproval {
                                next_agent: next,
                                context: None,
                            };
                            vec![]
                        } else {
                            state.current_agent = next;
                            vec![AppMessage::CreateSession {
                                task_id,
                                agent: next,
                                prompt: String::new(),
                                context: None,
                            }]
                        }
                    }
                    None => {
                        state.session_id = None;
                        state.phase = WorkflowPhase::PendingReview;
                        vec![]
                    }
                }
            }

            AppMessage::AgentKickedBack {
                task_id,
                from,
                to,
                reason,
            } => {
                let Some(state) = self.states.get_mut(&task_id) else {
                    return vec![];
                };
                if !from.valid_kickback_targets().contains(&to) {
                    let session_id = state.session_id.clone().unwrap_or_default();
                    return vec![AppMessage::SessionError {
                        task_id,
                        session_id,
                        error: format!(
                            "Invalid kickback from {} to {}: not a valid target",
                            from.display_name(),
                            to.display_name()
                        ),
                    }];
                }
                state.session_id = None;
                if self.approval_gate_enabled {
                    state.phase = WorkflowPhase::AwaitingApproval {
                        next_agent: to,
                        context: Some(reason),
                    };
                    vec![]
                } else {
                    state.current_agent = to;
                    vec![AppMessage::CreateSession {
                        task_id,
                        agent: to,
                        prompt: String::new(),
                        context: Some(reason),
                    }]
                }
            }

            AppMessage::AgentAskedQuestion { task_id, .. } => {
                if let Some(state) = self.states.get_mut(&task_id) {
                    // TODO: Track question count per task -- currently assumes one question at a time
                    state.phase = WorkflowPhase::AwaitingAnswer { question_index: 0 };
                }
                vec![]
            }

            AppMessage::HumanAnswered {
                task_id,
                question_index,
                answer,
            } => {
                let Some(state) = self.states.get_mut(&task_id) else {
                    return vec![];
                };
                state.phase = WorkflowPhase::Running;
                let agent = state.current_agent;
                vec![AppMessage::CreateSession {
                    task_id,
                    agent,
                    prompt: String::new(),
                    context: Some(format!("Question {question_index} answer: {answer}")),
                }]
            }

            AppMessage::HumanApprovedReview { task_id } => {
                if let Some(state) = self.states.get_mut(&task_id) {
                    state.phase = WorkflowPhase::Completed;
                }
                vec![]
            }

            AppMessage::HumanRequestedRevisions { task_id, comments } => {
                let Some(state) = self.states.get_mut(&task_id) else {
                    return vec![];
                };
                state.current_agent = AgentKind::CodeReview;
                state.phase = WorkflowPhase::Running;
                state.session_id = None;
                vec![AppMessage::CreateSession {
                    task_id,
                    agent: AgentKind::CodeReview,
                    prompt: String::new(),
                    context: Some(comments.join("; ")),
                }]
            }

            AppMessage::HumanApprovedTransition { task_id } => {
                let Some(state) = self.states.get_mut(&task_id) else {
                    return vec![];
                };
                match &state.phase {
                    WorkflowPhase::AwaitingApproval {
                        next_agent,
                        context,
                    } => {
                        let next = *next_agent;
                        let ctx = context.clone();
                        state.current_agent = next;
                        state.phase = WorkflowPhase::Running;
                        vec![AppMessage::CreateSession {
                            task_id,
                            agent: next,
                            prompt: String::new(),
                            context: ctx,
                        }]
                    }
                    _ => vec![],
                }
            }

            AppMessage::SessionError {
                task_id,
                session_id: _,
                error,
            } => {
                if let Some(state) = self.states.get_mut(&task_id) {
                    state.phase = WorkflowPhase::Errored { reason: error };
                }
                vec![]
            }

            // All other variants are no-ops.
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str) -> TaskId {
        TaskId::from_path(format!("tasks/{id}.md"))
    }

    #[test]
    fn test_start_task_transitions_to_running() {
        let mut engine = WorkflowEngine::new(false);
        let tid = task("6.1");
        let msgs = engine.process(AppMessage::StartTask {
            task_id: tid.clone(),
        });
        let state = engine.state(&tid).expect("state should exist");
        assert_eq!(state.phase, WorkflowPhase::Running);
        assert_eq!(state.current_agent, AgentKind::Intake);
        assert_eq!(msgs.len(), 1);
        assert!(
            matches!(&msgs[0], AppMessage::CreateSession { agent, .. } if *agent == AgentKind::Intake)
        );
    }

    #[test]
    fn test_agent_completed_advances_pipeline() {
        let mut engine = WorkflowEngine::new(false);
        let tid = task("6.1");
        engine.process(AppMessage::StartTask {
            task_id: tid.clone(),
        });

        let pipeline = [
            AgentKind::Intake,
            AgentKind::Design,
            AgentKind::Planning,
            AgentKind::Implementation,
            AgentKind::CodeQuality,
            AgentKind::SecurityReview,
        ];

        for agent in &pipeline {
            let msgs = engine.process(AppMessage::AgentCompleted {
                task_id: tid.clone(),
                agent: *agent,
                summary: "done".to_string(),
            });
            assert_eq!(msgs.len(), 1, "agent {agent:?} should emit CreateSession");
            let next = agent.next().expect("should have next");
            assert!(
                matches!(&msgs[0], AppMessage::CreateSession { agent: a, .. } if *a == next),
                "expected CreateSession for {next:?}"
            );
        }
    }

    #[test]
    fn test_agent_completed_ignored_for_mismatched_agent() {
        let mut engine = WorkflowEngine::new(false);
        let tid = task("6.1");
        engine.process(AppMessage::StartTask {
            task_id: tid.clone(),
        });
        // current_agent is Intake; fire AgentCompleted for a different agent
        let msgs = engine.process(AppMessage::AgentCompleted {
            task_id: tid.clone(),
            agent: AgentKind::Design,
            summary: "done".to_string(),
        });
        assert!(
            msgs.is_empty(),
            "mismatched AgentCompleted should be ignored"
        );
        let state = engine.state(&tid).expect("state");
        assert_eq!(
            state.current_agent,
            AgentKind::Intake,
            "agent should not advance on mismatch"
        );
        assert_eq!(
            state.phase,
            WorkflowPhase::Running,
            "phase should remain Running"
        );
    }

    #[test]
    fn test_code_review_completed_transitions_to_pending_review() {
        let mut engine = WorkflowEngine::new(false);
        let tid = task("6.1");
        engine.process(AppMessage::StartTask {
            task_id: tid.clone(),
        });
        // Advance through all 6 pipeline stages via SessionCompleted to reach CodeReview
        for _ in 0..6 {
            engine.process(AppMessage::SessionCompleted {
                task_id: tid.clone(),
                session_id: "s".to_string(),
                response_text: String::new(),
            });
        }

        let msgs = engine.process(AppMessage::AgentCompleted {
            task_id: tid.clone(),
            agent: AgentKind::CodeReview,
            summary: "done".to_string(),
        });
        assert!(
            msgs.is_empty(),
            "CodeReview completed should emit no messages"
        );
        let state = engine.state(&tid).expect("state");
        assert_eq!(state.phase, WorkflowPhase::PendingReview);
        assert_eq!(state.session_id, None, "session_id should be cleared");
    }

    #[test]
    fn test_valid_kickback_accepted() {
        let mut engine = WorkflowEngine::new(false);
        let tid = task("6.1");
        engine.process(AppMessage::StartTask {
            task_id: tid.clone(),
        });

        let msgs = engine.process(AppMessage::AgentKickedBack {
            task_id: tid.clone(),
            from: AgentKind::CodeQuality,
            to: AgentKind::Implementation,
            reason: "needs rework".to_string(),
        });
        assert_eq!(msgs.len(), 1);
        assert!(
            matches!(&msgs[0], AppMessage::CreateSession { agent, .. } if *agent == AgentKind::Implementation)
        );
        let state = engine.state(&tid).expect("state");
        assert_eq!(state.current_agent, AgentKind::Implementation);
    }

    #[test]
    fn test_invalid_kickback_rejected() {
        let mut engine = WorkflowEngine::new(false);
        let tid = task("6.1");
        engine.process(AppMessage::StartTask {
            task_id: tid.clone(),
        });

        let msgs = engine.process(AppMessage::AgentKickedBack {
            task_id: tid.clone(),
            from: AgentKind::Intake,
            to: AgentKind::Implementation,
            reason: "bad kickback".to_string(),
        });
        assert_eq!(msgs.len(), 1);
        assert!(matches!(&msgs[0], AppMessage::SessionError { .. }));
    }

    #[test]
    fn test_question_pauses_workflow() {
        let mut engine = WorkflowEngine::new(false);
        let tid = task("6.1");
        engine.process(AppMessage::StartTask {
            task_id: tid.clone(),
        });

        let msgs = engine.process(AppMessage::AgentAskedQuestion {
            task_id: tid.clone(),
            agent: AgentKind::Intake,
            question: "What is the scope?".to_string(),
        });
        assert!(msgs.is_empty());
        let state = engine.state(&tid).expect("state");
        assert_eq!(
            state.phase,
            WorkflowPhase::AwaitingAnswer { question_index: 0 }
        );
    }

    #[test]
    fn test_human_answer_resumes_workflow() {
        let mut engine = WorkflowEngine::new(false);
        let tid = task("6.1");
        engine.process(AppMessage::StartTask {
            task_id: tid.clone(),
        });
        engine.process(AppMessage::AgentAskedQuestion {
            task_id: tid.clone(),
            agent: AgentKind::Intake,
            question: "What is the scope?".to_string(),
        });

        let msgs = engine.process(AppMessage::HumanAnswered {
            task_id: tid.clone(),
            question_index: 0,
            answer: "Full scope".to_string(),
        });
        assert_eq!(msgs.len(), 1);
        assert!(matches!(&msgs[0], AppMessage::CreateSession { .. }));
        let state = engine.state(&tid).expect("state");
        assert_eq!(state.phase, WorkflowPhase::Running);
    }

    #[test]
    fn test_human_approved_completes() {
        let mut engine = WorkflowEngine::new(false);
        let tid = task("6.1");
        engine.process(AppMessage::StartTask {
            task_id: tid.clone(),
        });
        // Advance through the full 7-step pipeline via SessionCompleted to reach PendingReview
        for _ in 0..7 {
            engine.process(AppMessage::SessionCompleted {
                task_id: tid.clone(),
                session_id: "s".to_string(),
                response_text: String::new(),
            });
        }

        let msgs = engine.process(AppMessage::HumanApprovedReview {
            task_id: tid.clone(),
        });
        assert!(msgs.is_empty());
        let state = engine.state(&tid).expect("state");
        assert_eq!(state.phase, WorkflowPhase::Completed);
    }

    #[test]
    fn test_session_error_transitions_to_errored() {
        let mut engine = WorkflowEngine::new(false);
        let tid = task("6.1");
        engine.process(AppMessage::StartTask {
            task_id: tid.clone(),
        });

        let msgs = engine.process(AppMessage::SessionError {
            task_id: tid.clone(),
            session_id: "sess-1".to_string(),
            error: "out of memory".to_string(),
        });
        assert!(msgs.is_empty());
        let state = engine.state(&tid).expect("state");
        assert_eq!(
            state.phase,
            WorkflowPhase::Errored {
                reason: "out of memory".to_string()
            }
        );
    }

    #[test]
    fn test_session_created_records_session_id() {
        let mut engine = WorkflowEngine::new(false);
        let tid = task("6.1");
        engine.process(AppMessage::StartTask {
            task_id: tid.clone(),
        });

        engine.process(AppMessage::SessionCreated {
            task_id: tid.clone(),
            session_id: "sess-42".to_string(),
        });
        let state = engine.state(&tid).expect("state");
        assert_eq!(state.session_id, Some("sess-42".to_string()));
    }

    #[test]
    fn test_human_requested_revisions() {
        let mut engine = WorkflowEngine::new(false);
        let tid = task("6.1");
        engine.process(AppMessage::StartTask {
            task_id: tid.clone(),
        });

        let msgs = engine.process(AppMessage::HumanRequestedRevisions {
            task_id: tid.clone(),
            comments: vec!["Fix formatting".to_string(), "Add tests".to_string()],
        });
        assert_eq!(msgs.len(), 1);
        assert!(
            matches!(&msgs[0], AppMessage::CreateSession { agent, .. } if *agent == AgentKind::CodeReview)
        );
        let state = engine.state(&tid).expect("state");
        assert_eq!(state.current_agent, AgentKind::CodeReview);
        assert_eq!(state.phase, WorkflowPhase::Running);
    }

    #[test]
    fn test_unhandled_message_ignored() {
        let mut engine = WorkflowEngine::new(false);
        let msgs = engine.process(AppMessage::Tick);
        assert!(msgs.is_empty());
    }

    #[test]
    fn test_create_session_carries_context_on_kickback() {
        let mut engine = WorkflowEngine::new(false);
        let tid = task("6.1");
        engine.process(AppMessage::StartTask {
            task_id: tid.clone(),
        });

        let msgs = engine.process(AppMessage::AgentKickedBack {
            task_id: tid.clone(),
            from: AgentKind::CodeQuality,
            to: AgentKind::Implementation,
            reason: "needs rework".to_string(),
        });
        assert_eq!(msgs.len(), 1);
        assert!(
            matches!(
                &msgs[0],
                AppMessage::CreateSession { context: Some(ctx), .. } if ctx == "needs rework"
            ),
            "kickback CreateSession should carry the reason as context"
        );
    }

    #[test]
    fn test_create_session_carries_context_on_answer() {
        let mut engine = WorkflowEngine::new(false);
        let tid = task("6.1");
        engine.process(AppMessage::StartTask {
            task_id: tid.clone(),
        });
        engine.process(AppMessage::AgentAskedQuestion {
            task_id: tid.clone(),
            agent: AgentKind::Intake,
            question: "Scope?".to_string(),
        });

        let msgs = engine.process(AppMessage::HumanAnswered {
            task_id: tid.clone(),
            question_index: 0,
            answer: "Full scope".to_string(),
        });
        assert_eq!(msgs.len(), 1);
        assert!(
            matches!(
                &msgs[0],
                AppMessage::CreateSession { context: Some(ctx), .. }
                    if ctx.contains("Full scope")
            ),
            "answer CreateSession should carry the answer as context"
        );
    }

    // --- Approval gate tests ---

    #[test]
    fn test_approval_gate_pauses_on_agent_completed() {
        let mut engine = WorkflowEngine::new(true);
        let tid = task("6.1");
        engine.process(AppMessage::StartTask {
            task_id: tid.clone(),
        });

        let msgs = engine.process(AppMessage::AgentCompleted {
            task_id: tid.clone(),
            agent: AgentKind::Intake,
            summary: "done".to_string(),
        });
        assert!(
            msgs.is_empty(),
            "gate enabled: AgentCompleted should emit no messages"
        );
        let state = engine.state(&tid).expect("state");
        assert_eq!(
            state.current_agent,
            AgentKind::Intake,
            "current_agent should remain Intake until approved"
        );
        assert!(
            matches!(
                &state.phase,
                WorkflowPhase::AwaitingApproval {
                    next_agent: AgentKind::Design,
                    context: None
                }
            ),
            "phase should be AwaitingApproval for Design"
        );
    }

    #[test]
    fn test_approval_gate_resume_creates_session() {
        let mut engine = WorkflowEngine::new(true);
        let tid = task("6.1");
        engine.process(AppMessage::StartTask {
            task_id: tid.clone(),
        });
        engine.process(AppMessage::AgentCompleted {
            task_id: tid.clone(),
            agent: AgentKind::Intake,
            summary: "done".to_string(),
        });

        let msgs = engine.process(AppMessage::HumanApprovedTransition {
            task_id: tid.clone(),
        });
        assert_eq!(msgs.len(), 1, "approval should emit CreateSession");
        assert!(
            matches!(&msgs[0], AppMessage::CreateSession { agent, .. } if *agent == AgentKind::Design),
            "CreateSession should target Design agent"
        );
        let state = engine.state(&tid).expect("state");
        assert_eq!(state.current_agent, AgentKind::Design);
        assert_eq!(state.phase, WorkflowPhase::Running);
    }

    #[test]
    fn test_approval_gate_disabled_advances_immediately() {
        let mut engine = WorkflowEngine::new(false);
        let tid = task("6.1");
        engine.process(AppMessage::StartTask {
            task_id: tid.clone(),
        });

        let msgs = engine.process(AppMessage::AgentCompleted {
            task_id: tid.clone(),
            agent: AgentKind::Intake,
            summary: "done".to_string(),
        });
        assert_eq!(
            msgs.len(),
            1,
            "gate disabled: should emit CreateSession immediately"
        );
        assert!(
            matches!(&msgs[0], AppMessage::CreateSession { agent, .. } if *agent == AgentKind::Design)
        );
        let state = engine.state(&tid).expect("state");
        assert_eq!(state.current_agent, AgentKind::Design);
        assert_eq!(state.phase, WorkflowPhase::Running);
    }

    #[test]
    fn test_approval_gate_pauses_on_kickback() {
        let mut engine = WorkflowEngine::new(true);
        let tid = task("6.1");
        engine.process(AppMessage::StartTask {
            task_id: tid.clone(),
        });

        let msgs = engine.process(AppMessage::AgentKickedBack {
            task_id: tid.clone(),
            from: AgentKind::CodeQuality,
            to: AgentKind::Implementation,
            reason: "needs rework".to_string(),
        });
        assert!(
            msgs.is_empty(),
            "gate enabled: kickback should emit no messages"
        );
        let state = engine.state(&tid).expect("state");
        assert!(
            matches!(
                &state.phase,
                WorkflowPhase::AwaitingApproval {
                    next_agent: AgentKind::Implementation,
                    context: Some(ctx),
                } if ctx == "needs rework"
            ),
            "phase should be AwaitingApproval with kickback reason as context"
        );
    }

    #[test]
    fn test_approval_gate_resume_preserves_kickback_context() {
        let mut engine = WorkflowEngine::new(true);
        let tid = task("6.1");
        engine.process(AppMessage::StartTask {
            task_id: tid.clone(),
        });
        engine.process(AppMessage::AgentKickedBack {
            task_id: tid.clone(),
            from: AgentKind::CodeQuality,
            to: AgentKind::Implementation,
            reason: "needs rework".to_string(),
        });

        let msgs = engine.process(AppMessage::HumanApprovedTransition {
            task_id: tid.clone(),
        });
        assert_eq!(msgs.len(), 1);
        assert!(
            matches!(
                &msgs[0],
                AppMessage::CreateSession {
                    agent,
                    context: Some(ctx),
                    ..
                } if *agent == AgentKind::Implementation && ctx == "needs rework"
            ),
            "CreateSession should carry the kickback reason as context"
        );
    }

    #[test]
    fn test_approval_gate_no_pause_on_last_agent() {
        let mut engine = WorkflowEngine::new(true);
        let tid = task("6.1");
        engine.process(AppMessage::StartTask {
            task_id: tid.clone(),
        });
        // Advance to CodeReview (last agent) via SessionCompleted with gate disabled temporarily.
        // Simpler: advance via AgentCompleted with gate enabled and approve each step.
        // Use gate disabled to reach CodeReview quickly, then re-enable.
        engine.set_approval_gate(false);
        for _ in 0..6 {
            engine.process(AppMessage::SessionCompleted {
                task_id: tid.clone(),
                session_id: "s".to_string(),
                response_text: String::new(),
            });
        }
        engine.set_approval_gate(true);

        let msgs = engine.process(AppMessage::AgentCompleted {
            task_id: tid.clone(),
            agent: AgentKind::CodeReview,
            summary: "done".to_string(),
        });
        assert!(
            msgs.is_empty(),
            "CodeReview has no next agent, should transition to PendingReview, not AwaitingApproval"
        );
        let state = engine.state(&tid).expect("state");
        assert_eq!(
            state.phase,
            WorkflowPhase::PendingReview,
            "should be PendingReview, not AwaitingApproval"
        );
    }

    // --- Resume tests ---

    #[test]
    fn test_resume_creates_session_at_specified_agent() {
        let mut engine = WorkflowEngine::new(false);
        let tid = task("6.1");
        let msgs = engine.resume(tid.clone(), AgentKind::Implementation);
        assert_eq!(msgs.len(), 1);
        assert!(
            matches!(
                &msgs[0],
                AppMessage::CreateSession { agent, .. } if *agent == AgentKind::Implementation
            ),
            "resume should emit CreateSession for the specified agent"
        );
        let state = engine.state(&tid).expect("state should exist after resume");
        assert_eq!(state.current_agent, AgentKind::Implementation);
        assert_eq!(state.phase, WorkflowPhase::Running);
        assert!(state.session_id.is_none());
    }

    #[test]
    fn test_resume_replaces_errored_state() {
        let mut engine = WorkflowEngine::new(false);
        let tid = task("6.1");
        // Put the engine into Errored state.
        engine.process(AppMessage::StartTask {
            task_id: tid.clone(),
        });
        engine.process(AppMessage::SessionError {
            task_id: tid.clone(),
            session_id: "s1".to_string(),
            error: "boom".to_string(),
        });
        let errored = engine.state(&tid).expect("state");
        assert!(matches!(errored.phase, WorkflowPhase::Errored { .. }));
        // Resume should overwrite the Errored state.
        let msgs = engine.resume(tid.clone(), AgentKind::Design);
        assert_eq!(msgs.len(), 1);
        let state = engine.state(&tid).expect("state after resume");
        assert_eq!(state.current_agent, AgentKind::Design);
        assert_eq!(state.phase, WorkflowPhase::Running);
    }

    #[test]
    fn test_resume_at_intake_when_no_prior_state() {
        let mut engine = WorkflowEngine::new(false);
        let tid = task("6.1");
        // No prior state exists (simulates app crash scenario with fallback to Intake).
        let msgs = engine.resume(tid.clone(), AgentKind::Intake);
        assert_eq!(msgs.len(), 1);
        assert!(
            matches!(
                &msgs[0],
                AppMessage::CreateSession { agent, context: Some(ctx), .. }
                    if *agent == AgentKind::Intake && ctx.contains("resumed")
            ),
            "resume should emit CreateSession for Intake with resumed context"
        );
        let state = engine.state(&tid).expect("state");
        assert_eq!(state.current_agent, AgentKind::Intake);
        assert_eq!(state.phase, WorkflowPhase::Running);
    }

    #[test]
    fn test_human_approved_transition_ignored_when_not_awaiting() {
        let mut engine = WorkflowEngine::new(true);
        let tid = task("6.1");
        engine.process(AppMessage::StartTask {
            task_id: tid.clone(),
        });
        // Phase is Running, not AwaitingApproval.
        let msgs = engine.process(AppMessage::HumanApprovedTransition {
            task_id: tid.clone(),
        });
        assert!(
            msgs.is_empty(),
            "HumanApprovedTransition while Running should be a no-op"
        );
        let state = engine.state(&tid).expect("state");
        assert_eq!(state.phase, WorkflowPhase::Running);
        assert_eq!(state.current_agent, AgentKind::Intake);
    }
}
