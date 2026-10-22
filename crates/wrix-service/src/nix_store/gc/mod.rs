//! Scoped collection through already-running owners; acquisition never authorizes startup.

use std::{
    fmt, thread,
    time::{Duration, Instant},
};
use wrix_core::nix_store::pool::{Key, Repository};

const RETRY: Duration = Duration::from_millis(10);

/// One finite maintenance acquisition budget per pool, not a collection deadline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Wait(Duration);

impl Default for Wait {
    fn default() -> Self {
        Self(Duration::from_secs(30))
    }
}

impl Wait {
    pub fn parse(value: &str) -> Result<Self, WaitError> {
        let duration = wrix_core::duration::parse(value)?;
        Instant::now()
            .checked_add(duration)
            .ok_or(WaitError::Overflow)?;
        Ok(Self(duration))
    }

    pub const fn duration(self) -> Duration {
        self.0
    }
}

#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum WaitError {
    /// {0}
    Duration(#[from] wrix_core::duration::ParseError),
    /// collection wait exceeds the monotonic clock range
    Overflow,
}

/// Successful acquisition retains native maintenance exclusion through collection.
pub enum Attempt<Guard, Error> {
    Acquired(Guard),
    Busy,
    Unavailable(Error),
}

/// Inventory must distinguish authenticated empty scope from an unavailable owner.
/// Implementations may not provision, repair or restart a backend to obtain access.
pub trait Backend {
    type Guard;
    type Error: fmt::Display;

    fn pools(&self, repository: &Repository) -> Result<Vec<Key>, Self::Error>;
    /// Make one nonblocking attempt without starting an owner or interrupting peer work.
    fn acquire(&self, pool: &Key) -> Attempt<Self::Guard, Self::Error>;
    fn collect(&self, guard: Self::Guard) -> Result<(), Self::Error>;
}

#[derive(Debug, Eq, PartialEq)]
pub enum Outcome {
    Collected,
    Incomplete(Failure),
}

impl fmt::Display for Outcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Collected => formatter.write_str("collected"),
            Self::Incomplete(failure) => write!(formatter, "incomplete: {failure}"),
        }
    }
}

#[derive(Debug, Eq, PartialEq, displaydoc::Display)]
pub enum Failure {
    /// maintenance access remained busy until its acquisition deadline
    Contended,
    /// native owner unavailable: {0}
    Unavailable(String),
    /// native collection failed: {0}
    Collection(String),
    /// acquisition deadline exceeds the monotonic clock range
    ClockRange,
}

#[derive(Debug, Eq, PartialEq)]
pub struct Unit {
    pub pool: Key,
    pub outcome: Outcome,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub struct Report {
    pub units: Vec<Unit>,
}

impl Report {
    pub fn complete(&self) -> bool {
        self.units
            .iter()
            .all(|unit| unit.outcome == Outcome::Collected)
    }
}

#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum Error {
    /// installed-store collection inventory unavailable: {0}
    Inventory(String),
    /// installed-store collection inventory contains a peer repository or duplicate pool; no collection was dispatched
    Scope,
}

/// The caller must retain checked host scope through inventory and all native effects.
/// No collection is dispatched until the entire inventory's scope is validated.
pub fn run(backend: &impl Backend, repository: &Repository, wait: Wait) -> Result<Report, Error> {
    let pools = backend
        .pools(repository)
        .map_err(|error| Error::Inventory(error.to_string()))?;
    for (index, pool) in pools.iter().enumerate() {
        if &pool.repository != repository || pools[..index].contains(pool) {
            return Err(Error::Scope);
        }
    }
    let units = pools
        .into_iter()
        .map(|pool| {
            let outcome = collect(backend, &pool, wait);
            Unit { pool, outcome }
        })
        .collect();
    Ok(Report { units })
}

fn collect(backend: &impl Backend, pool: &Key, wait: Wait) -> Outcome {
    let Some(deadline) = Instant::now().checked_add(wait.duration()) else {
        return Outcome::Incomplete(Failure::ClockRange);
    };
    loop {
        match backend.acquire(pool) {
            Attempt::Acquired(guard) => {
                return match backend.collect(guard) {
                    Ok(()) => Outcome::Collected,
                    Err(error) => Outcome::Incomplete(Failure::Collection(error.to_string())),
                };
            }
            Attempt::Unavailable(error) => {
                return Outcome::Incomplete(Failure::Unavailable(error.to_string()));
            }
            Attempt::Busy => (),
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Outcome::Incomplete(Failure::Contended);
        }
        thread::sleep(RETRY.min(remaining));
        if Instant::now() >= deadline {
            return Outcome::Incomplete(Failure::Contended);
        }
    }
}

/// No native pool owner transport has been admitted by this package.
pub(crate) struct Unavailable;

impl Backend for Unavailable {
    type Guard = std::convert::Infallible;
    type Error = &'static str;

    fn pools(&self, _repository: &Repository) -> Result<Vec<Key>, Self::Error> {
        Err(
            "no admitted running pool owner; installed-store collection is unavailable in this version",
        )
    }

    fn acquire(&self, _pool: &Key) -> Attempt<Self::Guard, Self::Error> {
        Attempt::Unavailable("no admitted running pool owner")
    }

    fn collect(&self, guard: Self::Guard) -> Result<(), Self::Error> {
        match guard {}
    }
}
