//! Markdown task file parser.
//!
//! Uses a two-phase line-oriented approach:
//! - Phase 1: parse `Key: Value` metadata lines before the first `##` heading.
//! - Phase 2: split on `##` headings; parse known sections and preserve unknown
//!   ones verbatim for round-trip fidelity.
//!
//! Task 1.5 implements the full parser.

//TODO: Task 1.5 -- implement TaskParser with parse(path) -> Result<Task> method
