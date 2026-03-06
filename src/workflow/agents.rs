//! AgentKind enum and pipeline ordering.
//!
//! Defines the 7 agents in the ClawMux pipeline and provides methods for
//! pipeline navigation (`next`, `prev`), ordering (`pipeline_index`),
//! kickback validation (`valid_kickback_targets`), and name conversion
//! (`Display`, `FromStr`, `opencode_agent_name`).

use std::fmt;
use std::str::FromStr;

use crate::error::ClawMuxError;

/// The 7 pipeline agents plus special `Human`, `ResearchPlan`, and `ResearchAct` markers.
///
/// Pipeline agents are applied sequentially:
/// `Intake` -> `Design` -> `Planning` -> `Implementation`
/// -> `CodeQuality` -> `SecurityReview` -> `CodeReview`.
///
/// Review-stage agents (`CodeQuality`, `SecurityReview`, `CodeReview`) may kick
/// tasks back to earlier stages when issues are found.
///
/// `Human` is not part of the automated pipeline; it represents assignment to a
/// human reviewer and is used only in task file metadata.
///
/// `ResearchPlan` and `ResearchAct` are special out-of-pipeline agents backing the
/// Research tab scratchpad. They share the same task_id but differ in tool permissions
/// and system prompt. `ResearchPlan` is read-only and focuses on analysis; `ResearchAct`
/// has full tool access for execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentKind {
    /// Gathers initial context and clarifies requirements.
    Intake,
    /// Produces a design for the task.
    Design,
    /// Produces an implementation plan.
    Planning,
    /// Implements the code changes.
    Implementation,
    /// Reviews code for quality, style, and correctness.
    CodeQuality,
    /// Audits code for security vulnerabilities.
    SecurityReview,
    /// Performs a final review before human approval.
    CodeReview,
    /// Represents assignment to a human reviewer (not part of the automated pipeline).
    Human,
    /// Backs the Research tab in read-only Plan mode; not part of the task pipeline.
    ResearchPlan,
    /// Backs the Research tab in full-tool Act mode; not part of the task pipeline.
    ResearchAct,
}

#[allow(dead_code)]
impl AgentKind {
    /// Returns the opencode agent name string for this variant.
    ///
    /// Equivalent to `self.to_string()` but returns a `&str` without allocation.
    pub fn opencode_agent_name(&self) -> &str {
        match self {
            AgentKind::Intake => "clawmux/intake",
            AgentKind::Design => "clawmux/design",
            AgentKind::Planning => "clawmux/planning",
            AgentKind::Implementation => "clawmux/implementation",
            AgentKind::CodeQuality => "clawmux/code-quality",
            AgentKind::SecurityReview => "clawmux/security-review",
            AgentKind::CodeReview => "clawmux/code-review",
            AgentKind::Human => "human",
            AgentKind::ResearchPlan => "clawmux/research-plan",
            AgentKind::ResearchAct => "clawmux/research-act",
        }
    }

    /// Returns the kiro-cli agent name string for this variant.
    ///
    /// Kiro agents are named `"clawmux-<stage>"` and correspond to JSON config
    /// files in `.kiro/agents/` scaffolded by `clawmux init`.
    pub fn kiro_agent_name(&self) -> &str {
        match self {
            AgentKind::Intake => "clawmux-intake",
            AgentKind::Design => "clawmux-design",
            AgentKind::Planning => "clawmux-planning",
            AgentKind::Implementation => "clawmux-implementation",
            AgentKind::CodeQuality => "clawmux-code-quality",
            AgentKind::SecurityReview => "clawmux-security-review",
            AgentKind::CodeReview => "clawmux-code-review",
            AgentKind::Human => "human",
            AgentKind::ResearchPlan => "clawmux-research-plan",
            AgentKind::ResearchAct => "clawmux-research-act",
        }
    }

    /// Returns the zero-based index of this agent in the pipeline.
    ///
    /// `Intake` is 0, `CodeReview` is 6. `Human` returns 7 (outside the pipeline).
    /// `ResearchPlan` and `ResearchAct` both return 8 (out-of-pipeline scratchpad).
    pub fn pipeline_index(&self) -> usize {
        match self {
            AgentKind::Intake => 0,
            AgentKind::Design => 1,
            AgentKind::Planning => 2,
            AgentKind::Implementation => 3,
            AgentKind::CodeQuality => 4,
            AgentKind::SecurityReview => 5,
            AgentKind::CodeReview => 6,
            AgentKind::Human => 7,
            AgentKind::ResearchPlan | AgentKind::ResearchAct => 8,
        }
    }

    /// Returns the next agent in the pipeline, or `None` if this is the last.
    ///
    /// The pipeline order is:
    /// `Intake` -> `Design` -> `Planning` -> `Implementation`
    /// -> `CodeQuality` -> `SecurityReview` -> `CodeReview` -> (end).
    /// `Human`, `ResearchPlan`, and `ResearchAct` are not part of the pipeline
    /// and always return `None`.
    pub fn next(&self) -> Option<AgentKind> {
        match self {
            AgentKind::Intake => Some(AgentKind::Design),
            AgentKind::Design => Some(AgentKind::Planning),
            AgentKind::Planning => Some(AgentKind::Implementation),
            AgentKind::Implementation => Some(AgentKind::CodeQuality),
            AgentKind::CodeQuality => Some(AgentKind::SecurityReview),
            AgentKind::SecurityReview => Some(AgentKind::CodeReview),
            AgentKind::CodeReview
            | AgentKind::Human
            | AgentKind::ResearchPlan
            | AgentKind::ResearchAct => None,
        }
    }

    /// Returns the previous agent in the pipeline, or `None` if this is the first.
    ///
    /// Inverse of [`next`][AgentKind::next].
    /// `Human`, `ResearchPlan`, and `ResearchAct` are not part of the pipeline
    /// and always return `None`.
    pub fn prev(&self) -> Option<AgentKind> {
        match self {
            AgentKind::Intake
            | AgentKind::Human
            | AgentKind::ResearchPlan
            | AgentKind::ResearchAct => None,
            AgentKind::Design => Some(AgentKind::Intake),
            AgentKind::Planning => Some(AgentKind::Design),
            AgentKind::Implementation => Some(AgentKind::Planning),
            AgentKind::CodeQuality => Some(AgentKind::Implementation),
            AgentKind::SecurityReview => Some(AgentKind::CodeQuality),
            AgentKind::CodeReview => Some(AgentKind::SecurityReview),
        }
    }

    /// Returns the set of agents this agent is allowed to kick a task back to.
    ///
    /// Only review-stage agents may initiate kickbacks:
    /// - `CodeQuality` may kick back to `Implementation`.
    /// - `SecurityReview` may kick back to `Implementation` or `Design`.
    /// - `CodeReview` may kick back to `Implementation`, `Design`, or `Planning`.
    /// - All other agents (including `Human`, `ResearchPlan`, `ResearchAct`) return an empty slice.
    pub fn valid_kickback_targets(&self) -> &'static [AgentKind] {
        match self {
            AgentKind::CodeQuality => &[AgentKind::Implementation],
            AgentKind::SecurityReview => &[AgentKind::Implementation, AgentKind::Design],
            AgentKind::CodeReview => &[
                AgentKind::Implementation,
                AgentKind::Design,
                AgentKind::Planning,
            ],
            _ => &[],
        }
    }

    /// Returns a static slice of all 7 pipeline agents in pipeline order.
    ///
    /// The order is: `Intake`, `Design`, `Planning`, `Implementation`,
    /// `CodeQuality`, `SecurityReview`, `CodeReview`.
    ///
    /// Note: `Human`, `ResearchPlan`, and `ResearchAct` are intentionally excluded
    /// as they are not pipeline agents.
    pub fn all() -> &'static [AgentKind] {
        &[
            AgentKind::Intake,
            AgentKind::Design,
            AgentKind::Planning,
            AgentKind::Implementation,
            AgentKind::CodeQuality,
            AgentKind::SecurityReview,
            AgentKind::CodeReview,
        ]
    }

    /// Returns a static slice of the two research agent variants.
    ///
    /// Used when deploying research agent definition files during `clawmux init`.
    pub fn research_agents() -> &'static [AgentKind] {
        &[AgentKind::ResearchPlan, AgentKind::ResearchAct]
    }

    /// Returns the human-friendly display name for this agent.
    ///
    /// Used in task file metadata (e.g., `Assigned To: [Planning Agent]`).
    ///
    /// | Variant         | Display name            |
    /// |-----------------|-------------------------|
    /// | Intake          | "Intake Agent"          |
    /// | Design          | "Design Agent"          |
    /// | Planning        | "Planning Agent"        |
    /// | Implementation  | "Implementation Agent"  |
    /// | CodeQuality     | "Code Quality Agent"    |
    /// | SecurityReview  | "Security Review Agent" |
    /// | CodeReview      | "Code Review Agent"     |
    /// | Human           | "Human"                 |
    /// | ResearchPlan    | "Research (Plan)"       |
    /// | ResearchAct     | "Research (Act)"        |
    pub fn display_name(&self) -> &'static str {
        match self {
            AgentKind::Intake => "Intake Agent",
            AgentKind::Design => "Design Agent",
            AgentKind::Planning => "Planning Agent",
            AgentKind::Implementation => "Implementation Agent",
            AgentKind::CodeQuality => "Code Quality Agent",
            AgentKind::SecurityReview => "Security Review Agent",
            AgentKind::CodeReview => "Code Review Agent",
            AgentKind::Human => "Human",
            AgentKind::ResearchPlan => "Research (Plan)",
            AgentKind::ResearchAct => "Research (Act)",
        }
    }

    /// Parses a human-friendly display name (case-insensitive) into an `AgentKind`.
    ///
    /// Accepts the full form (`"Planning Agent"`) or the short form without the
    /// `" Agent"` suffix (`"Planning"`). `"Human"` is also accepted.
    /// `"research"` maps to `ResearchPlan` for backwards compatibility.
    ///
    /// # Errors
    ///
    /// Returns [`ClawMuxError::Parse`] if the string does not match any known display name.
    pub fn from_display_name(s: &str) -> crate::error::Result<AgentKind> {
        // Strip trailing " agent" suffix before matching (case-insensitive).
        let lower = s.trim().to_lowercase();
        let short = lower
            .strip_suffix(" agent")
            .unwrap_or(lower.as_str())
            .trim();
        match short {
            "intake" => Ok(AgentKind::Intake),
            "design" => Ok(AgentKind::Design),
            "planning" => Ok(AgentKind::Planning),
            "implementation" => Ok(AgentKind::Implementation),
            "code quality" => Ok(AgentKind::CodeQuality),
            "security review" => Ok(AgentKind::SecurityReview),
            "code review" => Ok(AgentKind::CodeReview),
            "human" => Ok(AgentKind::Human),
            // "research" kept for backwards compat; maps to Plan mode.
            "research" | "research (plan)" => Ok(AgentKind::ResearchPlan),
            "research (act)" => Ok(AgentKind::ResearchAct),
            other => Err(ClawMuxError::Parse {
                file: "<agent kind>".to_string(),
                message: format!("unknown agent display name: '{other}'"),
            }),
        }
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.opencode_agent_name())
    }
}

impl FromStr for AgentKind {
    type Err = ClawMuxError;

    /// Parses an opencode agent name string into an `AgentKind`.
    ///
    /// Parsing is case-insensitive. The expected format is `"clawmux/<stage>"`.
    /// `"clawmux/research"` maps to `ResearchPlan` for backwards compatibility.
    ///
    /// # Errors
    ///
    /// Returns [`ClawMuxError::Parse`] if the string does not match any known agent name.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "clawmux/intake" => Ok(AgentKind::Intake),
            "clawmux/design" => Ok(AgentKind::Design),
            "clawmux/planning" => Ok(AgentKind::Planning),
            "clawmux/implementation" => Ok(AgentKind::Implementation),
            "clawmux/code-quality" => Ok(AgentKind::CodeQuality),
            "clawmux/security-review" => Ok(AgentKind::SecurityReview),
            "clawmux/code-review" => Ok(AgentKind::CodeReview),
            "human" => Ok(AgentKind::Human),
            // "clawmux/research" kept for backwards compat; maps to Plan mode.
            "clawmux/research" | "clawmux/research-plan" => Ok(AgentKind::ResearchPlan),
            "clawmux/research-act" => Ok(AgentKind::ResearchAct),
            other => Err(ClawMuxError::Parse {
                file: "<agent kind>".to_string(),
                message: format!("unknown agent name: '{other}'"),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pipeline_order() {
        assert_eq!(AgentKind::Intake.pipeline_index(), 0);
        assert_eq!(AgentKind::Design.pipeline_index(), 1);
        assert_eq!(AgentKind::Planning.pipeline_index(), 2);
        assert_eq!(AgentKind::Implementation.pipeline_index(), 3);
        assert_eq!(AgentKind::CodeQuality.pipeline_index(), 4);
        assert_eq!(AgentKind::SecurityReview.pipeline_index(), 5);
        assert_eq!(AgentKind::CodeReview.pipeline_index(), 6);
        assert_eq!(AgentKind::ResearchPlan.pipeline_index(), 8);
        assert_eq!(AgentKind::ResearchAct.pipeline_index(), 8);
    }

    #[test]
    fn test_next_chain() {
        let mut current = AgentKind::Intake;
        let expected = [
            AgentKind::Design,
            AgentKind::Planning,
            AgentKind::Implementation,
            AgentKind::CodeQuality,
            AgentKind::SecurityReview,
            AgentKind::CodeReview,
        ];
        for next_expected in &expected {
            let next = current.next().expect("should have a next agent");
            assert_eq!(&next, next_expected);
            current = next;
        }
        assert_eq!(current, AgentKind::CodeReview);
        assert_eq!(current.next(), None, "CodeReview should have no next");
    }

    #[test]
    fn test_prev_chain() {
        let mut current = AgentKind::CodeReview;
        let expected = [
            AgentKind::SecurityReview,
            AgentKind::CodeQuality,
            AgentKind::Implementation,
            AgentKind::Planning,
            AgentKind::Design,
            AgentKind::Intake,
        ];
        for prev_expected in &expected {
            let prev = current.prev().expect("should have a previous agent");
            assert_eq!(&prev, prev_expected);
            current = prev;
        }
        assert_eq!(current, AgentKind::Intake);
        assert_eq!(current.prev(), None, "Intake should have no prev");
    }

    #[test]
    fn test_valid_kickback_targets() {
        assert_eq!(
            AgentKind::CodeQuality.valid_kickback_targets(),
            &[AgentKind::Implementation]
        );
        assert_eq!(
            AgentKind::SecurityReview.valid_kickback_targets(),
            &[AgentKind::Implementation, AgentKind::Design]
        );
        assert_eq!(
            AgentKind::CodeReview.valid_kickback_targets(),
            &[
                AgentKind::Implementation,
                AgentKind::Design,
                AgentKind::Planning
            ]
        );
    }

    #[test]
    fn test_intake_no_kickback() {
        assert!(AgentKind::Intake.valid_kickback_targets().is_empty());
    }

    #[test]
    fn test_display() {
        assert_eq!(AgentKind::Intake.to_string(), "clawmux/intake");
        assert_eq!(AgentKind::Design.to_string(), "clawmux/design");
        assert_eq!(AgentKind::Planning.to_string(), "clawmux/planning");
        assert_eq!(
            AgentKind::Implementation.to_string(),
            "clawmux/implementation"
        );
        assert_eq!(AgentKind::CodeQuality.to_string(), "clawmux/code-quality");
        assert_eq!(
            AgentKind::SecurityReview.to_string(),
            "clawmux/security-review"
        );
        assert_eq!(AgentKind::CodeReview.to_string(), "clawmux/code-review");
    }

    #[test]
    fn test_from_str() {
        assert_eq!(
            "clawmux/code-quality".parse::<AgentKind>().unwrap(),
            AgentKind::CodeQuality
        );
        // Case-insensitive
        assert_eq!(
            "CLAWMUX/CODE-QUALITY".parse::<AgentKind>().unwrap(),
            AgentKind::CodeQuality
        );
        // Unknown name returns error
        let err = "clawmux/unknown".parse::<AgentKind>().unwrap_err();
        assert!(matches!(err, ClawMuxError::Parse { file, .. } if file == "<agent kind>"));
    }

    #[test]
    fn test_display_name() {
        assert_eq!(AgentKind::Intake.display_name(), "Intake Agent");
        assert_eq!(AgentKind::Design.display_name(), "Design Agent");
        assert_eq!(AgentKind::Planning.display_name(), "Planning Agent");
        assert_eq!(
            AgentKind::Implementation.display_name(),
            "Implementation Agent"
        );
        assert_eq!(AgentKind::CodeQuality.display_name(), "Code Quality Agent");
        assert_eq!(
            AgentKind::SecurityReview.display_name(),
            "Security Review Agent"
        );
        assert_eq!(AgentKind::CodeReview.display_name(), "Code Review Agent");
        assert_eq!(AgentKind::Human.display_name(), "Human");
    }

    #[test]
    fn test_from_display_name_full() {
        assert_eq!(
            AgentKind::from_display_name("Intake Agent").unwrap(),
            AgentKind::Intake
        );
        assert_eq!(
            AgentKind::from_display_name("Code Quality Agent").unwrap(),
            AgentKind::CodeQuality
        );
        assert_eq!(
            AgentKind::from_display_name("Human").unwrap(),
            AgentKind::Human
        );
    }

    #[test]
    fn test_from_display_name_short() {
        assert_eq!(
            AgentKind::from_display_name("Intake").unwrap(),
            AgentKind::Intake
        );
        assert_eq!(
            AgentKind::from_display_name("Code Quality").unwrap(),
            AgentKind::CodeQuality
        );
        assert_eq!(
            AgentKind::from_display_name("Human").unwrap(),
            AgentKind::Human
        );
    }

    #[test]
    fn test_from_display_name_case_insensitive() {
        assert_eq!(
            AgentKind::from_display_name("intake agent").unwrap(),
            AgentKind::Intake
        );
        assert_eq!(
            AgentKind::from_display_name("DESIGN AGENT").unwrap(),
            AgentKind::Design
        );
    }

    #[test]
    fn test_from_display_name_invalid() {
        let err = AgentKind::from_display_name("Unknown Agent").unwrap_err();
        assert!(matches!(err, ClawMuxError::Parse { .. }));
    }

    #[test]
    fn test_human_pipeline_index() {
        assert_eq!(AgentKind::Human.pipeline_index(), 7);
    }

    #[test]
    fn test_human_not_in_all() {
        assert!(!AgentKind::all().contains(&AgentKind::Human));
    }

    #[test]
    fn test_human_from_str() {
        assert_eq!("human".parse::<AgentKind>().unwrap(), AgentKind::Human);
    }

    #[test]
    fn test_research_plan_variant() {
        assert_eq!(AgentKind::ResearchPlan.pipeline_index(), 8);
        assert_eq!(
            AgentKind::ResearchPlan.opencode_agent_name(),
            "clawmux/research-plan"
        );
        assert_eq!(
            AgentKind::ResearchPlan.kiro_agent_name(),
            "clawmux-research-plan"
        );
        assert_eq!(AgentKind::ResearchPlan.display_name(), "Research (Plan)");
        assert_eq!(AgentKind::ResearchPlan.next(), None);
        assert_eq!(AgentKind::ResearchPlan.prev(), None);
        assert!(AgentKind::ResearchPlan.valid_kickback_targets().is_empty());
        assert!(!AgentKind::all().contains(&AgentKind::ResearchPlan));
        assert_eq!(
            "clawmux/research-plan".parse::<AgentKind>().unwrap(),
            AgentKind::ResearchPlan
        );
        assert_eq!(
            AgentKind::from_display_name("research").unwrap(),
            AgentKind::ResearchPlan,
            "backwards compat: 'research' maps to ResearchPlan"
        );
        assert_eq!(
            AgentKind::from_display_name("research (plan)").unwrap(),
            AgentKind::ResearchPlan
        );
        assert_eq!(AgentKind::ResearchPlan.to_string(), "clawmux/research-plan");
        // Backwards compat: old "clawmux/research" string parses to ResearchPlan.
        assert_eq!(
            "clawmux/research".parse::<AgentKind>().unwrap(),
            AgentKind::ResearchPlan
        );
    }

    #[test]
    fn test_research_act_variant() {
        assert_eq!(AgentKind::ResearchAct.pipeline_index(), 8);
        assert_eq!(
            AgentKind::ResearchAct.opencode_agent_name(),
            "clawmux/research-act"
        );
        assert_eq!(
            AgentKind::ResearchAct.kiro_agent_name(),
            "clawmux-research-act"
        );
        assert_eq!(AgentKind::ResearchAct.display_name(), "Research (Act)");
        assert_eq!(AgentKind::ResearchAct.next(), None);
        assert_eq!(AgentKind::ResearchAct.prev(), None);
        assert!(AgentKind::ResearchAct.valid_kickback_targets().is_empty());
        assert!(!AgentKind::all().contains(&AgentKind::ResearchAct));
        assert_eq!(
            "clawmux/research-act".parse::<AgentKind>().unwrap(),
            AgentKind::ResearchAct
        );
        assert_eq!(
            AgentKind::from_display_name("research (act)").unwrap(),
            AgentKind::ResearchAct
        );
        assert_eq!(AgentKind::ResearchAct.to_string(), "clawmux/research-act");
    }

    #[test]
    fn test_research_agents_slice() {
        let agents = AgentKind::research_agents();
        assert_eq!(agents.len(), 2);
        assert!(agents.contains(&AgentKind::ResearchPlan));
        assert!(agents.contains(&AgentKind::ResearchAct));
    }

    #[test]
    fn test_kiro_agent_name() {
        assert_eq!(AgentKind::Intake.kiro_agent_name(), "clawmux-intake");
        assert_eq!(AgentKind::Design.kiro_agent_name(), "clawmux-design");
        assert_eq!(AgentKind::Planning.kiro_agent_name(), "clawmux-planning");
        assert_eq!(
            AgentKind::Implementation.kiro_agent_name(),
            "clawmux-implementation"
        );
        assert_eq!(
            AgentKind::CodeQuality.kiro_agent_name(),
            "clawmux-code-quality"
        );
        assert_eq!(
            AgentKind::SecurityReview.kiro_agent_name(),
            "clawmux-security-review"
        );
        assert_eq!(
            AgentKind::CodeReview.kiro_agent_name(),
            "clawmux-code-review"
        );
        assert_eq!(AgentKind::Human.kiro_agent_name(), "human");
    }
}
