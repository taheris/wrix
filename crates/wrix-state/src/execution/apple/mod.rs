//! Creation-time-bound Apple VM evidence, independent of Podman container IDs.
mod recorded;
use super::{Context, Error, Host, Phase, Store, Ticket};
use crate::store::Generation;
pub use recorded::Recorded;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Wire", into = "Wire")]
pub struct Identity {
    name: Name,
    born: Birth,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    name: String,
    born: f64,
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct Name(String);
#[derive(Clone, Copy, Debug)]
struct Birth(f64);
impl PartialEq for Birth {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}
impl Eq for Birth {}
impl Identity {
    pub fn parse(name: &str, born: f64) -> Result<Self, Error> {
        if name.is_empty()
            || name.len() > 63
            || !name.as_bytes()[0].is_ascii_alphanumeric()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
            || !born.is_finite()
            || born <= 0.0
        {
            return Err(Error::Unsupported);
        }
        Ok(Self {
            name: Name(name.to_owned()),
            born: Birth(born),
        })
    }
    pub fn name(&self) -> &str {
        &self.name.0
    }
    pub const fn born(&self) -> f64 {
        self.born.0
    }
}
impl TryFrom<Wire> for Identity {
    type Error = Error;
    fn try_from(value: Wire) -> Result<Self, Error> {
        Self::parse(&value.name, value.born)
    }
}
impl From<Identity> for Wire {
    fn from(value: Identity) -> Self {
        Self {
            name: value.name.0,
            born: value.born.0,
        }
    }
}

/// One-shot original admission bound to the exact native creation, not its reusable name alone.
pub struct Bound {
    host: Arc<Host>,
    generation: Generation,
    context: Context,
    ticket: Ticket,
    identity: Identity,
}
impl Bound {
    pub(super) const fn new(
        host: Arc<Host>,
        generation: Generation,
        context: Context,
        ticket: Ticket,
        identity: Identity,
    ) -> Self {
        Self {
            host,
            generation,
            context,
            ticket,
            identity,
        }
    }
    pub const fn identity(&self) -> &Identity {
        &self.identity
    }
    pub const fn ticket(&self) -> &Ticket {
        &self.ticket
    }
    pub fn recheck(&self) -> Result<(), Error> {
        self.context.recheck(&self.host)
    }
    pub fn recorded(&self) -> Result<Recorded, Error> {
        let record = Recorded::load(
            self.host.clone(),
            self.context.key(),
            &self.ticket,
            &self.generation,
        )?
        .ok_or(Error::Conflict)?;
        if record.identity() != &self.identity {
            return Err(Error::Conflict);
        }
        Ok(record)
    }
    pub fn observe(&self) -> Result<Phase, Error> {
        let phase =
            super::observe(&self.host, &self.context, &self.ticket)?.ok_or(Error::Conflict)?;
        match &phase {
            Phase::AppleBound(id) | Phase::AppleRetired(id) if id == &self.identity => Ok(phase),
            _ => Err(Error::Conflict),
        }
    }
}

#[cfg(test)]
mod test;
