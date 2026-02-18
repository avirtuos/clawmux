//! App configuration loading and opencode agent definition management.
//!
//! Loads global config (`~/.config/clawdmux/config.toml`) and project config
//! (`.clawdmux/config.toml`), and manages the lifecycle of opencode agent
//! definition files in `.opencode/agents/clawdmux/`.
//! Task 2.1 implements the full config module.

pub mod init;
pub mod providers;
