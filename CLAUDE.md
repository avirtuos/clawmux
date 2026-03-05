
# Project Overview

ClawMux is a GenAI coding assistance multiplexer and task orchestrator. It allows you to manage a list of scrum style stories and tasks, assign them to individual GenAI coding assistance sessions, and interact with those sessions all from a single tool.

## Rules

1.  If you are working on a task, first read the task details as well as the parent story's details.
2. Prioritize idiomatic Rust. Always strive for clear and concise rust code. Follow Rust's best practices, including ownership, borrowing, and error handling.
3. At the end of each task make sure to update the task status as well as the documentation (e.g. README.md and docs/design.md) with any relevant changes.
4. Every new feature or bug fix should be accompanied by appropriate unit and integration tests. Aim for test coverage above 70%.
5. When validating a change or completion of a test, always run `cargo fmt`, `cargo build`, and `cargo test` and `cargo clippy`. There should be no test failures, build errors, build warnings, or clippy errors/warnings at the end of a task.
6. Maintain clear documentation, add doc comments (`///`) to public items (structs, enums, functions, etc.) explaining their purpose, arguments, and return values.
9. Any stub implementations or temporary code (e.g. hard coded return values) should be clearly marked with a //TODO comment and you should get approval before doing so.
10. Do not remove or mark tests as ignored without explaining why the change is needed and asking for approval before doing so.
11. Do not create new documents (e.g. design docs) without asking first.
12. Use tracing subscriber (e.g. info!) for logging instead of print or eprint statements. Do not use any special characters in log or print statements, only regular alpha-numeric and punctuation characters.

Agents working on this project MUST adhere to these rules.