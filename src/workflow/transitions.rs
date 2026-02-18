//! State transition logic for the agent pipeline.
//!
//! Validates and applies state transitions, including kickback validation.
//! Given a `(current_state, message)` pair, produces `(new_state, Vec<AppMessage>)`
//! side effects. This pure design makes the engine trivially testable.
//! Task 1.4 implements the full transition logic.

//TODO: Task 1.4 -- implement WorkflowEngine state machine transitions
