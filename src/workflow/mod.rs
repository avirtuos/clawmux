//! Workflow engine: agent pipeline state machine.
//!
//! Drives tasks through the 7-agent pipeline (Intake -> Design -> Planning ->
//! Implementation -> CodeQuality -> SecurityReview -> CodeReview), handling
//! forward advancement, kickbacks, and question pauses.

pub mod agents;
pub mod prompt_composer;
pub mod transitions;

// TODO: Remove #[allow(unused_imports)] once the message dispatcher consumes these
#[allow(unused_imports)]
pub use transitions::{WorkflowEngine, WorkflowPhase, WorkflowState};
