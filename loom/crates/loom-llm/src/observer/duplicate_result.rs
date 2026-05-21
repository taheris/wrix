//! `DuplicateResultObserver` — pure observability. Detects any tool
//! result whose canonical-JSON payload duplicates an earlier result in
//! the same session, regardless of which tool produced it. Surfaces a
//! wasted-token signal for SaaS billing pipelines without ever sending
//! a `SessionCommand`.

use loom_events::{AgentEvent, EventSink, SessionCommand};

use super::hash::ResultHasher;

/// Default `min_bytes` threshold below which short results are skipped
/// — keeps the dedup map from being dominated by trivially-short
/// payloads like `"ok"`.
pub const DEFAULT_MIN_BYTES: u32 = 256;

/// Observer state for one session. State resets on `CompactionEnd`.
pub struct DuplicateResultObserver {
    /// Shared canonicalization + hashing pipeline (same instance shape
    /// the `DoomLoopObserver` uses).
    hasher: ResultHasher,
    /// Skip results below this byte count.
    min_bytes: u32,
}

impl DuplicateResultObserver {
    /// Construct an observer with documented defaults
    /// (`min_bytes = 256`).
    pub fn new() -> Self {
        Self {
            hasher: ResultHasher::new(),
            min_bytes: DEFAULT_MIN_BYTES,
        }
    }

    /// Override the `min_bytes` threshold.
    pub fn with_min_bytes(mut self, n: u32) -> Self {
        self.min_bytes = n;
        self
    }

    /// Borrow the shared hasher.
    pub fn hasher(&self) -> &ResultHasher {
        &self.hasher
    }

    /// Read-only access to the configured threshold.
    pub fn min_bytes(&self) -> u32 {
        self.min_bytes
    }
}

impl Default for DuplicateResultObserver {
    fn default() -> Self {
        Self::new()
    }
}

impl EventSink for DuplicateResultObserver {
    fn emit(&mut self, _event: &AgentEvent) {}

    fn react(&mut self) -> Vec<SessionCommand> {
        Vec::new()
    }
}
