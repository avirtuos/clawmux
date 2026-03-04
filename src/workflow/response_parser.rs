//! Parses structured JSON responses emitted by pipeline agents.
//!
//! Each agent must end its session with a JSON object containing an `"action"` field
//! (one of `"complete"`, `"question"`, `"kickback"`). The JSON may be embedded in
//! free-form text, so this module attempts a direct parse first and falls back to
//! extracting the last balanced-brace JSON object from the text.

use serde::Deserialize;

use crate::error::{ClawdMuxError, Result};

/// Optional document updates that an agent may include with a `complete` response.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct AgentUpdates {
    /// Updated design notes from the Design agent.
    pub design: Option<String>,
    /// Updated implementation plan from the Planning agent.
    pub implementation_plan: Option<String>,
}

/// A structured response produced by a pipeline agent at the end of its session.
///
/// Agents must serialize one of these variants as JSON so that ClawdMux can
/// route the task to the next pipeline step, ask the human a question, or
/// send the task back to an earlier agent.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "action")]
pub enum AgentResponse {
    /// The agent finished its work and the pipeline should advance.
    #[serde(rename = "complete")]
    Complete {
        /// A short summary of what was accomplished.
        summary: String,
        /// Optional document updates (design, implementation_plan).
        #[serde(default)]
        updates: Option<AgentUpdates>,
        /// Optional commit message for the agent's changes.
        #[serde(default)]
        commit_message: Option<String>,
    },
    /// The agent needs a human answer before it can continue.
    #[serde(rename = "question")]
    Question {
        /// The question text to display to the human.
        question: String,
        /// Additional context explaining why this question is being asked.
        context: String,
    },
    /// The agent is returning the task to an earlier pipeline agent.
    #[serde(rename = "kickback")]
    Kickback {
        /// Display name of the target agent (e.g. `"Implementation Agent"`).
        target_agent: String,
        /// Explanation of what needs to be fixed.
        reason: String,
    },
}

/// Extracts the last balanced-brace JSON substring starting at `start` in `s`.
///
/// Scans forward from `start`, counting `{` and `}` characters (ignoring those
/// inside string literals) until the braces balance. Returns a slice of `s`
/// from `start` to the closing `}`, or `None` if the braces never balance.
fn extract_balanced_json(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escape = false;

    for (i, &b) in bytes[start..].iter().enumerate() {
        if escape {
            escape = false;
            continue;
        }
        match b {
            b'\\' if in_string => escape = true,
            b'"' => in_string = !in_string,
            b'{' if !in_string => depth += 1,
            b'}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..start + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Parses a pipeline agent response from accumulated session text.
///
/// The function first tries to parse `text` directly as an [`AgentResponse`].
/// If that fails, it searches backwards through `text` for the last `"action"`
/// key, backtracks to the nearest `{` that opens the enclosing object, and
/// attempts to extract and parse the balanced-brace JSON object starting there.
/// This handles both compact (`{"action":"complete",...}`) and pretty-printed
/// (`{\n  "action": "complete",\n  ...\n}`) agent output.
///
/// # Errors
///
/// Returns [`ClawdMuxError::Json`] if no valid [`AgentResponse`] JSON can be found.
pub fn parse_response(text: &str) -> Result<AgentResponse> {
    let trimmed = text.trim();

    // Fast path: the entire text is a valid AgentResponse JSON.
    if let Ok(response) = serde_json::from_str::<AgentResponse>(trimmed) {
        return Ok(response);
    }

    // Fallback: find the last "action" key and backtrack to the enclosing '{'.
    // Using rfind("\"action\"") handles both compact and pretty-printed JSON.
    if let Some(action_pos) = trimmed.rfind("\"action\"") {
        let prefix = &trimmed[..action_pos];
        if let Some(brace_pos) = prefix.rfind('{') {
            let candidate = &trimmed[brace_pos..];
            if let Some(json_str) = extract_balanced_json(candidate) {
                if let Ok(response) = serde_json::from_str::<AgentResponse>(json_str) {
                    return Ok(response);
                }
            }
        }
    }

    Err(ClawdMuxError::Json(
        serde_json::from_str::<AgentResponse>("{}").unwrap_err(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_complete_bare() {
        let json = r#"{"action":"complete","summary":"All done"}"#;
        let resp = parse_response(json).expect("should parse");
        assert!(
            matches!(resp, AgentResponse::Complete { ref summary, .. } if summary == "All done")
        );
    }

    #[test]
    fn test_parse_complete_with_design_update() {
        let json =
            r#"{"action":"complete","summary":"done","updates":{"design":"New design notes"}}"#;
        let resp = parse_response(json).expect("should parse");
        if let AgentResponse::Complete {
            updates: Some(u), ..
        } = resp
        {
            assert_eq!(u.design.as_deref(), Some("New design notes"));
        } else {
            panic!("expected Complete with updates");
        }
    }

    #[test]
    fn test_parse_complete_with_impl_plan() {
        let json =
            r#"{"action":"complete","summary":"done","updates":{"implementation_plan":"Step 1"}}"#;
        let resp = parse_response(json).expect("should parse");
        if let AgentResponse::Complete {
            updates: Some(u), ..
        } = resp
        {
            assert_eq!(u.implementation_plan.as_deref(), Some("Step 1"));
        } else {
            panic!("expected Complete with impl plan");
        }
    }

    #[test]
    fn test_parse_complete_with_commit_message() {
        let json = r#"{"action":"complete","summary":"done","commit_message":"feat: add feature"}"#;
        let resp = parse_response(json).expect("should parse");
        assert!(
            matches!(resp, AgentResponse::Complete { commit_message: Some(ref m), .. } if m == "feat: add feature")
        );
    }

    #[test]
    fn test_parse_question() {
        let json = r#"{"action":"question","question":"What is the scope?","context":"Need clarification"}"#;
        let resp = parse_response(json).expect("should parse");
        assert!(
            matches!(resp, AgentResponse::Question { ref question, .. } if question == "What is the scope?")
        );
    }

    #[test]
    fn test_parse_kickback() {
        let json = r#"{"action":"kickback","target_agent":"Implementation Agent","reason":"Missing tests"}"#;
        let resp = parse_response(json).expect("should parse");
        assert!(
            matches!(resp, AgentResponse::Kickback { ref target_agent, .. } if target_agent == "Implementation Agent")
        );
    }

    #[test]
    fn test_parse_embedded_json() {
        let text = "Here is my analysis...\n\nSome commentary.\n\n{\"action\":\"complete\",\"summary\":\"Finished\"}\n\nThat is all.";
        let resp = parse_response(text).expect("should parse embedded JSON");
        assert!(
            matches!(resp, AgentResponse::Complete { ref summary, .. } if summary == "Finished")
        );
    }

    #[test]
    fn test_parse_pretty_printed_json() {
        let text = "Some preamble text.\n\n{\n  \"action\": \"complete\",\n  \"summary\": \"Done\",\n  \"updates\": {\n    \"implementation_plan\": \"Step 1\"\n  }\n}";
        let resp = parse_response(text).expect("should parse pretty-printed JSON");
        if let AgentResponse::Complete {
            ref summary,
            updates: Some(ref u),
            ..
        } = resp
        {
            assert_eq!(summary, "Done");
            assert_eq!(u.implementation_plan.as_deref(), Some("Step 1"));
        } else {
            panic!("expected Complete with updates, got {resp:?}");
        }
    }

    #[test]
    fn test_parse_no_json() {
        let text = "I could not complete the task.";
        let result = parse_response(text);
        assert!(result.is_err(), "should fail with no JSON");
    }

    #[test]
    fn test_parse_malformed_json() {
        let text = r#"{"action":"complete","summary":}"#;
        let result = parse_response(text);
        assert!(result.is_err(), "should fail with malformed JSON");
    }

    #[test]
    fn test_parse_unknown_action() {
        let text = r#"{"action":"unknown","foo":"bar"}"#;
        let result = parse_response(text);
        assert!(result.is_err(), "unknown action should fail to parse");
    }
}
