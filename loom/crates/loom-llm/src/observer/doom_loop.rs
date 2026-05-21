//! `DoomLoopObserver` — detects when the agent calls the same tool
//! with the same params *and* the same result repeated, then escalates
//! via a two-stage `Steer` -> `Abort` response.
//!
//! Detection keys on `(CallKey, ResultHash)` where `CallKey =
//! (tool_name, canonical_params)` (canonical JSON per RFC 8785 JCS,
//! normalized numbers) and `ResultHash` is `BLAKE3-16(canonical
//! result)` — shared with `DuplicateResultObserver` via
//! [`super::result_hasher::ResultHasher`].

use loom_events::{AgentEvent, EventSink, SessionCommand};

use super::result_hasher::ResultHasher;

/// Observer state for one session. Resets on `CompactionEnd`; does NOT
/// reset on `TurnEnd` — agent doom loops routinely span turns and
/// compaction is the actual context reset.
pub struct DoomLoopObserver {
    /// Per-call canonicalization + hashing pipeline.
    hasher: ResultHasher,
    /// Sliding-window size for the 3-of-5 detection rule.
    window: u32,
    /// Identical-pair count within the window that fires stage 1.
    threshold: u32,
    /// Additional identical pairs required after stage 1 before stage 2
    /// fires; configurable per the `stage_2_after_stage_1` knob.
    stage_2_after_stage_1: u32,
    /// Pending commands the loop drains via `react()` after each
    /// non-streaming event.
    pending: Vec<SessionCommand>,
}

impl DoomLoopObserver {
    /// Construct an observer with documented defaults: 3-of-5 window,
    /// 3 additional pairs after stage 1 before stage 2.
    pub fn new() -> Self {
        Self {
            hasher: ResultHasher::new(),
            window: 5,
            threshold: 3,
            stage_2_after_stage_1: 3,
            pending: Vec::new(),
        }
    }

    /// Override the sliding-window size.
    pub fn with_window(mut self, n: u32) -> Self {
        self.window = n;
        self
    }

    /// Override the identical-pair threshold for stage 1.
    pub fn with_threshold(mut self, n: u32) -> Self {
        self.threshold = n;
        self
    }

    /// Override the additional-pair gap before stage 2 fires.
    pub fn with_stage_2_after_stage_1(mut self, n: u32) -> Self {
        self.stage_2_after_stage_1 = n;
        self
    }

    /// Borrow the shared hasher. Held as a struct field so concrete
    /// detection logic in follow-up beads consumes the same pipeline as
    /// [`super::duplicate_result::DuplicateResultObserver`].
    pub fn hasher(&self) -> &ResultHasher {
        &self.hasher
    }

    /// Read-only access to the configured window size.
    pub fn window(&self) -> u32 {
        self.window
    }

    /// Read-only access to the configured stage-1 threshold.
    pub fn threshold(&self) -> u32 {
        self.threshold
    }

    /// Read-only access to the configured stage-2 gap.
    pub fn stage_2_after_stage_1(&self) -> u32 {
        self.stage_2_after_stage_1
    }
}

impl Default for DoomLoopObserver {
    fn default() -> Self {
        Self::new()
    }
}

impl EventSink for DoomLoopObserver {
    fn emit(&mut self, _event: &AgentEvent) {}

    fn react(&mut self) -> Vec<SessionCommand> {
        std::mem::take(&mut self.pending)
    }
}
