//! Native admission and work accounting; transport idleness is never completion.
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Serving,
    Draining,
    Releasing,
    Released,
}

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub phase: Phase,
    pub requests: usize,
    pub workflows: usize,
    pub background: usize,
    pub failed: bool,
}

struct Inner {
    phase: Phase,
    requests: usize,
    workflows: HashMap<String, String>,
    background: usize,
    failed: bool,
}

pub struct Tracker(Mutex<Inner>);

impl Tracker {
    pub fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(Inner {
            phase: Phase::Serving,
            requests: 0,
            workflows: HashMap::new(),
            background: 0,
            failed: false,
        })))
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        match self.0.lock() {
            Ok(inner) => inner,
            Err(poisoned) => {
                let mut inner = poisoned.into_inner();
                inner.failed = true;
                inner
            }
        }
    }

    pub fn status(&self) -> Status {
        let inner = self.lock();
        Status {
            phase: inner.phase,
            requests: inner.requests,
            workflows: inner.workflows.len(),
            background: inner.background,
            failed: inner.failed,
        }
    }

    pub fn begin(&self) -> bool {
        let mut inner = self.lock();
        match inner.phase {
            Phase::Serving | Phase::Draining => {
                inner.phase = Phase::Draining;
                true
            }
            Phase::Releasing | Phase::Released => false,
        }
    }

    pub fn abort(&self) -> bool {
        let mut inner = self.lock();
        match inner.phase {
            Phase::Serving | Phase::Draining => {
                inner.phase = Phase::Serving;
                true
            }
            Phase::Releasing | Phase::Released => false,
        }
    }

    pub fn quiescent(&self) -> bool {
        let inner = self.lock();
        inner.phase == Phase::Draining
            && inner.requests == 0
            && inner.workflows.is_empty()
            && inner.background == 0
            && !inner.failed
    }

    pub fn release(&self) -> bool {
        let mut inner = self.lock();
        if inner.phase != Phase::Draining
            || inner.requests != 0
            || !inner.workflows.is_empty()
            || inner.background != 0
            || inner.failed
        {
            return false;
        }
        inner.phase = Phase::Releasing;
        true
    }

    pub fn released(&self) {
        self.lock().phase = Phase::Released;
    }

    pub fn fail(&self) {
        self.lock().failed = true;
    }

    pub fn open_workflow(&self, id: String, authority: String) -> bool {
        let mut inner = self.lock();
        if inner.phase != Phase::Serving || inner.workflows.contains_key(&id) {
            return false;
        }
        inner.workflows.insert(id, authority);
        true
    }

    pub fn close_workflow(&self, id: &str, authority: &str) -> bool {
        let mut inner = self.lock();
        if inner
            .workflows
            .get(id)
            .is_none_or(|owner| owner != authority)
        {
            return false;
        }
        inner.workflows.remove(id);
        true
    }

    pub fn request(self: &Arc<Self>, workflow: Option<&str>, authority: &str) -> Option<Guard> {
        let mut inner = self.lock();
        let admitted = workflow.map_or_else(
            || inner.phase == Phase::Serving,
            |id| {
                inner
                    .workflows
                    .get(id)
                    .is_some_and(|owner| owner == authority)
            },
        );
        if !admitted || matches!(inner.phase, Phase::Releasing | Phase::Released) {
            return None;
        }
        inner.requests += 1;
        Some(Guard {
            tracker: self.clone(),
            kind: Kind::Request,
        })
    }

    /// Detached work must acquire this before its parent request releases its guard.
    pub fn background(self: &Arc<Self>) -> Guard {
        self.lock().background += 1;
        Guard {
            tracker: self.clone(),
            kind: Kind::Background,
        }
    }

    pub fn maintenance(self: &Arc<Self>) -> Option<Guard> {
        let mut inner = self.lock();
        if inner.phase != Phase::Serving {
            return None;
        }
        inner.background += 1;
        Some(Guard {
            tracker: self.clone(),
            kind: Kind::Background,
        })
    }
}

enum Kind {
    Request,
    Background,
}
pub struct Guard {
    tracker: Arc<Tracker>,
    kind: Kind,
}
impl Guard {
    /// Lost originating output is uncertain even after native execution finishes.
    pub fn uncertain(self) {
        self.tracker.fail();
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        let mut inner = self.tracker.lock();
        if std::thread::panicking() {
            inner.failed = true;
        }
        match self.kind {
            Kind::Request => inner.requests -= 1,
            Kind::Background => inner.background -= 1,
        }
    }
}
