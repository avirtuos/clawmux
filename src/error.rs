//! Centralized error type for ClawdMux.
//!
//! All subsystems return `Result<T, ClawdMuxError>` (via the [`Result`] alias).
//! Use the appropriate variant for each error category rather than converting
//! everything to [`ClawdMuxError::Internal`].

use thiserror::Error;

/// The primary error type for ClawdMux.
///
/// Covers all error categories produced by the application's subsystems.
/// Variants with `#[from]` support automatic conversion via the `?` operator.
#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum ClawdMuxError {
    /// An I/O error from the standard library (file, pipe, socket, etc.).
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// A parse error with the source file path and a human-readable message.
    #[error("Parse error in '{file}': {message}")]
    Parse {
        /// Path to the file that failed to parse.
        file: String,
        /// Description of what went wrong.
        message: String,
    },

    /// A serialization error, containing a description of the failure.
    #[error("Serialization error: {0}")]
    Serialize(String),

    /// A TOML deserialization error when reading config files.
    #[error("Config error: {0}")]
    Config(#[from] toml::de::Error),

    /// An API-level error returned by the opencode server, containing the error message.
    #[error("API error: {0}")]
    Api(String),

    /// An HTTP transport error from the `reqwest` client.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// An error parsing or consuming a Server-Sent Events stream.
    #[error("SSE error: {0}")]
    Sse(String),

    /// An error managing the opencode server process lifecycle.
    #[error("Server error: {0}")]
    Server(String),

    /// An error in the workflow engine or agent pipeline state machine.
    #[error("Workflow error: {0}")]
    Workflow(String),

    /// A JSON serialization/deserialization error from `serde_json`.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// An internal error that does not fit another category.
    #[error("Internal error: {0}")]
    Internal(String),
}

/// Convenience alias for `Result<T, ClawdMuxError>`.
#[allow(dead_code)]
pub type Result<T> = std::result::Result<T, ClawdMuxError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_io_error_from_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err: ClawdMuxError = io_err.into();
        assert!(matches!(err, ClawdMuxError::Io(_)));
    }

    #[test]
    fn test_json_error_from_conversion() {
        let json_err = serde_json::from_str::<serde_json::Value>("not valid json")
            .expect_err("should fail to parse");
        let err: ClawdMuxError = json_err.into();
        assert!(matches!(err, ClawdMuxError::Json(_)));
    }

    #[test]
    fn test_parse_error_display() {
        let err = ClawdMuxError::Parse {
            file: "tasks/story-1.md".to_string(),
            message: "missing Status field".to_string(),
        };
        let display = err.to_string();
        assert!(
            display.contains("tasks/story-1.md"),
            "display should contain file: {display}"
        );
        assert!(
            display.contains("missing Status field"),
            "display should contain message: {display}"
        );
    }

    #[test]
    fn test_result_alias() {
        fn dummy() -> crate::error::Result<u32> {
            Ok(1)
        }
        assert_eq!(dummy().unwrap(), 1);
    }
}
