//! Workflow engine: agent pipeline state machine.
//!
//! Drives tasks through the 7-agent pipeline (Intake -> Design -> Planning ->
//! Implementation -> CodeQuality -> SecurityReview -> CodeReview), handling
//! forward advancement, kickbacks, and question pauses.
//! Task 1.4 implements the full WorkflowEngine.

pub mod agents;
pub mod prompt_composer;
pub mod transitions;
