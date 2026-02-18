//! AgentKind enum and pipeline ordering.
//!
//! Defines the 7 agents in the ClawdMux pipeline.
//! Task 1.4 adds `impl AgentKind` methods: `next`, `pipeline_index`, and
//! `valid_kickback_targets`.

/// The 7 agents in the ClawdMux pipeline.
///
/// Agents are applied sequentially:
/// `Intake` -> `Design` -> `Planning` -> `Implementation`
/// -> `CodeQuality` -> `SecurityReview` -> `CodeReview`.
///
/// Review-stage agents (`CodeQuality`, `SecurityReview`, `CodeReview`) may kick
/// tasks back to earlier stages when issues are found.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[allow(dead_code)]
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
}
