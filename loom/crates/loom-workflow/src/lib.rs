//! Loom workflow engine.
//!
//! Implements the workflow phases (`plan`, `todo`, `run`, `check`, `msg`,
//! `spec`) on top of `loom-core`'s typed surface and `loom-templates`'
//! Askama-rendered prompts. Subsequent issues populate each phase module;
//! this crate currently exposes the skeleton only.
