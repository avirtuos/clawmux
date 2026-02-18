//! ProviderConfig: reads `~/.config/clawdmux/config.toml` and resolves credentials.
//!
//! Provides LLM provider API keys and model defaults to inject into the opencode
//! server process as environment variables (e.g., `ANTHROPIC_API_KEY`).
//! Credentials are never written to opencode's own config files.
//! Task 2.1 implements the full provider config.

//TODO: Task 2.1 -- implement ProviderConfig with load() and env_vars_for_opencode() -> HashMap<String, String>
