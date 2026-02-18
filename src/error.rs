//! Centralized error type for ClawdMux.
//!
//! Currently a placeholder struct. Task 1.2 converts this to a thiserror-derived enum.

/// The primary error type for ClawdMux.
///
/// This is a placeholder struct. Task 1.2 replaces it with a thiserror-derived enum
/// covering all subsystem error variants.
#[derive(Debug)]
#[allow(dead_code)]
pub struct ClawdMuxError;

/// Convenience alias for `Result<T, ClawdMuxError>`.
#[allow(dead_code)]
pub type Result<T> = std::result::Result<T, ClawdMuxError>;
