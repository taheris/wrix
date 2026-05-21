//! Sandbox-aware tool implementations registered with the in-process
//! `Conversation` by `loom-direct-runner`.
//!
//! Will host six tools — `Read`, `Write`, `Edit`, `Bash`, `Grep`, `Glob`
//! — each implementing the `Tool` trait from `loom-llm` and executing
//! against the container's bind-mounted workspace. See
//! `specs/loom-agent.md` § Direct Backend — *The six tools*.
