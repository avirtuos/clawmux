//! AppMessage enum -- the contract between ClawdMux subsystems.
//!
//! All inter-subsystem communication flows through this enum via async mpsc channels.
//! Variants cover terminal events, workflow commands, opencode session events,
//! diff events, task persistence, and application lifecycle.

use crossterm::event::Event;

use crate::opencode::types::{FileDiff, MessagePart, PermissionRequest};
use crate::tasks::models::TaskId;
use crate::workflow::agents::AgentKind;

/// All messages flowing between ClawdMux subsystems.
///
/// Every inter-subsystem interaction -- terminal input, workflow state changes,
/// opencode session lifecycle, diffs, task file updates, and shutdown -- is
/// represented as a variant of this enum and passed through async `mpsc` channels.
///
/// `AppMessage` intentionally does not implement `Clone`. Messages are consumed
/// by a single mpsc receiver; cloning would imply shared ownership that the
/// channel design does not support.
#[derive(Debug)]
#[allow(dead_code)]
pub enum AppMessage {
    // --- Terminal events ---
    /// A raw crossterm terminal event (key, mouse, resize, etc.).
    TerminalEvent(Event),
    /// Periodic timer tick used to drive UI refresh.
    Tick,

    // --- Workflow commands ---
    /// Instructs the workflow engine to begin processing the given task.
    StartTask { task_id: TaskId },
    /// Instructs the workflow engine to resume a previously interrupted task.
    ResumeTask { task_id: TaskId },
    /// Signals that an agent has finished its work on a task.
    AgentCompleted {
        task_id: TaskId,
        agent: AgentKind,
        summary: String,
    },
    /// Signals that an agent is kicking a task back to an earlier pipeline stage.
    AgentKickedBack {
        task_id: TaskId,
        from: AgentKind,
        to: AgentKind,
        reason: String,
    },
    /// Signals that an agent has a blocking question requiring human input.
    AgentAskedQuestion {
        task_id: TaskId,
        agent: AgentKind,
        question: String,
    },
    /// Carries the human's answer to a previously asked question.
    HumanAnswered {
        task_id: TaskId,
        /// Zero-based index into the task's `questions` list.
        question_index: usize,
        answer: String,
    },
    /// Signals that the human has approved the final code review.
    HumanApprovedReview { task_id: TaskId },
    /// Carries revision comments from the human reviewer.
    HumanRequestedRevisions {
        task_id: TaskId,
        comments: Vec<String>,
    },
    /// The human approved advancing to the next agent in the pipeline.
    HumanApprovedTransition { task_id: TaskId },

    // --- OpenCode session events ---
    /// Requests the OpenCode client to create a new session for the given agent.
    CreateSession {
        task_id: TaskId,
        agent: AgentKind,
        prompt: String,
        /// Semantic context for prompt composition (kickback reason, answer, revisions).
        context: Option<String>,
    },
    /// Confirms that a session was successfully created.
    SessionCreated { task_id: TaskId, session_id: String },
    /// Requests the OpenCode client to send an additional prompt to a running session.
    SendPrompt {
        task_id: TaskId,
        session_id: String,
        prompt: String,
    },
    /// Carries the current message parts streamed from an agent session.
    ///
    /// `parts` contains the **full current state** of the message (not a delta).
    /// The `message_id` field identifies which message is being updated so that
    /// the UI can replace previous content rather than append duplicates.
    StreamingUpdate {
        task_id: TaskId,
        session_id: String,
        message_id: String,
        parts: Vec<MessagePart>,
    },
    /// Reports a tool invocation status update within a session.
    ToolActivity {
        task_id: TaskId,
        session_id: String,
        tool: String,
        status: String,
        /// Human-readable summary of the tool's input (file path, command, etc.), if available.
        detail: Option<String>,
    },
    /// Confirms that a prompt was successfully sent to an active session.
    PromptSent { task_id: TaskId, session_id: String },
    /// Signals that a session was found idle during periodic liveness polling.
    ///
    /// Dispatched by the Tick-driven status poll. The `App` handler guards
    /// against false positives by checking that the workflow engine still has
    /// this exact session active in the Running phase.
    VerifySessionIdle {
        task_id: TaskId,
        session_id: String,
        /// Enriched error message from message listing, or a default.
        error: String,
    },
    /// Signals that a session has finished processing.
    SessionCompleted {
        task_id: TaskId,
        session_id: String,
        /// Accumulated assistant text from MessageUpdated events.
        response_text: String,
    },
    /// Signals that a session encountered an unrecoverable error.
    SessionError {
        task_id: TaskId,
        session_id: String,
        error: String,
    },
    /// Requests the OpenCode client to abort an active session.
    AbortSession { task_id: TaskId, session_id: String },

    // --- Permission events ---
    /// An OpenCode agent is requesting permission for a tool operation.
    PermissionAsked {
        task_id: TaskId,
        request: PermissionRequest,
    },
    /// The human has resolved a pending permission request.
    PermissionResolved {
        task_id: TaskId,
        request: PermissionRequest,
        /// One of "once", "always", or "reject".
        response: String,
        /// Optional explanation to send to the agent when rejecting.
        ///
        /// When present and non-empty (only meaningful for `response == "reject"`),
        /// the text is sent to the agent as a steering prompt so it can reconsider.
        explanation: Option<String>,
    },
    /// An OpenCode agent asked a question via the `question.asked` SSE event.
    ///
    /// Wired into the existing Questions tab (Tab 1).
    OpenCodeQuestionAsked {
        task_id: TaskId,
        request_id: String,
        question: String,
    },
    /// OpenCode reported that session diffs changed; poll the diff endpoint.
    SessionDiffChanged { task_id: TaskId, session_id: String },
    /// Updates the cumulative token counts for a task (input and output tokens).
    TokensUpdated {
        task_id: TaskId,
        input_tokens: u64,
        output_tokens: u64,
    },

    // --- Diff events ---
    /// Carries file diffs fetched from the opencode `/session/:id/diff` endpoint.
    DiffReady {
        task_id: TaskId,
        diffs: Vec<FileDiff>,
    },

    // --- Task persistence ---
    /// Signals that a task's in-memory state was updated and should be persisted.
    TaskUpdated { task_id: TaskId },
    /// Signals that a task's markdown file changed on disk (from an external editor).
    TaskFileChanged { task_id: TaskId },

    // --- Malformed task fix ---
    /// Requests an AI-generated fix suggestion for a malformed task file.
    RequestTaskFix { task_id: TaskId },
    /// Delivers a successful AI-generated fix suggestion.
    TaskFixReady {
        task_id: TaskId,
        corrected_content: String,
        explanation: String,
    },
    /// Reports that an AI fix request failed.
    TaskFixFailed { task_id: TaskId, error: String },
    /// Applies the pending fix suggestion for a malformed task.
    ApplyTaskFix { task_id: TaskId },

    // --- Application lifecycle ---
    /// Initiates a graceful application shutdown.
    Shutdown,
}
