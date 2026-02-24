//! Workflow engine: agent pipeline state machine.
//!
//! Drives tasks through the 7-agent pipeline (Intake -> Design -> Planning ->
//! Implementation -> CodeQuality -> SecurityReview -> CodeReview), handling
//! forward advancement, kickbacks, and question pauses.

pub mod agents;
pub mod prompt_composer;
pub mod response_parser;
pub mod transitions;

#[allow(unused_imports)]
pub use transitions::{WorkflowEngine, WorkflowPhase, WorkflowState};
