//! AppMessage enum -- the contract between ClawdMux subsystems.
//!
//! All inter-subsystem communication flows through this enum via async mpsc channels.
//! Task 1.4 replaces the placeholder variant with the full set of variants covering
//! terminal events, workflow commands, opencode session events, diff events,
//! task persistence, and application lifecycle.

/// All messages flowing between ClawdMux subsystems.
///
/// Variants cover terminal events, workflow commands, opencode session events,
/// diff events, task persistence, and application lifecycle.
#[derive(Debug)]
#[allow(dead_code)]
pub enum AppMessage {
    //TODO: Task 1.4 -- replace this placeholder with the full variant set
    /// Placeholder variant; will be removed in Task 1.4.
    Placeholder,
}
