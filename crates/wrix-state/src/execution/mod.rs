//! Host-private, one-shot runtime bindings; foreground exit is not teardown authority.

use crate::{
    context::Context,
    host::Host,
    store::{Error, Store},
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::io::Read;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Ticket(String);

impl Ticket {
    pub fn parse(value: &str) -> Result<Self, Error> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(Error::Unsupported);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn owner(&self) -> String {
        format!("execution/{}", self.0)
    }
}

impl<'de> Deserialize<'de> for Ticket {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Container(String);

impl Container {
    pub fn parse(value: &str) -> Result<Self, Error> {
        let ticket = Ticket::parse(value)?;
        Ok(Self(ticket.0))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Container {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum Phase {
    Reserved,
    Cancelled,
    Launching,
    Bound(Container),
    Retired(Container),
}

pub fn reserve(host: &Host, context: &Context) -> Result<Ticket, Error> {
    context.authorize_effects(host)?;
    let store = Store::mutate(host, false)?;
    context.check_store(host, Some(&store))?;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let digits = b"0123456789abcdef";
    let text: String = bytes
        .iter()
        .flat_map(|byte| {
            [
                char::from(digits[usize::from(byte >> 4)]),
                char::from(digits[usize::from(byte & 15)]),
            ]
        })
        .collect();
    let ticket = Ticket::parse(&text)?;
    store.connection.execute(
        "INSERT INTO namespace_metadata VALUES (?1, ?2, ?3)",
        params![
            context.key().as_str(),
            ticket.owner(),
            serde_json::to_vec(&Phase::Reserved).map_err(|_| Error::Unsupported)?
        ],
    )?;
    Ok(ticket)
}

pub fn observe(host: &Host, context: &Context, ticket: &Ticket) -> Result<Option<Phase>, Error> {
    context.recheck(host)?;
    let store = Store::inspect(host)?.ok_or(Error::Uninitialized)?;
    context.check_store(host, Some(&store))?;
    read(&store, context, ticket)
}

pub fn begin(host: &Host, context: &Context, ticket: &Ticket) -> Result<(), Error> {
    transition(host, context, ticket, &Phase::Reserved, &Phase::Launching)
}

pub fn bind(
    host: &Host,
    context: &Context,
    ticket: &Ticket,
    container: &Container,
) -> Result<(), Error> {
    transition(
        host,
        context,
        ticket,
        &Phase::Launching,
        &Phase::Bound(container.clone()),
    )
}

/// Called only after authoritative removal of this exact immutable runtime object.
pub fn retire(
    host: &Host,
    context: &Context,
    ticket: &Ticket,
    container: &Container,
) -> Result<(), Error> {
    transition(
        host,
        context,
        ticket,
        &Phase::Bound(container.clone()),
        &Phase::Retired(container.clone()),
    )
}

/// Revoke a never-handed-off reservation atomically; launching/unknown native work cannot be cancelled this way.
pub fn cancel(host: &Host, context: &Context, ticket: &Ticket) -> Result<(), Error> {
    transition(host, context, ticket, &Phase::Reserved, &Phase::Cancelled)
}

pub(crate) fn require_reserved(
    store: &Store,
    context: &Context,
    ticket: &Ticket,
) -> Result<(), Error> {
    if read(store, context, ticket)? != Some(Phase::Reserved) {
        return Err(Error::Conflict);
    }
    Ok(())
}

/// Host-native terminal evidence only; no caller-supplied exit status or containment ID can construct this.
pub struct Terminal {
    namespace: wrix_core::path::WorkspaceHash,
    ticket: Ticket,
    generation: crate::store::Generation,
}
impl Terminal {
    pub const fn namespace(&self) -> &wrix_core::path::WorkspaceHash {
        &self.namespace
    }
    pub const fn ticket(&self) -> &Ticket {
        &self.ticket
    }
    pub const fn generation(&self) -> &crate::store::Generation {
        &self.generation
    }
}

pub fn terminal(
    host: &Host,
    namespace: &wrix_core::path::WorkspaceHash,
    ticket: &Ticket,
) -> Result<Option<Terminal>, Error> {
    let store = Store::inspect(host)?.ok_or(Error::Uninitialized)?;
    if !matches!(
        read_namespace(&store, namespace, ticket)?,
        Some(Phase::Retired(_) | Phase::Cancelled)
    ) {
        return Ok(None);
    }
    Ok(Some(Terminal {
        namespace: namespace.clone(),
        ticket: ticket.clone(),
        generation: store.generation()?,
    }))
}

fn read(store: &Store, context: &Context, ticket: &Ticket) -> Result<Option<Phase>, Error> {
    read_namespace(store, context.key(), ticket)
}

fn read_namespace(
    store: &Store,
    namespace: &wrix_core::path::WorkspaceHash,
    ticket: &Ticket,
) -> Result<Option<Phase>, Error> {
    let bytes: Option<Vec<u8>> = store
        .connection
        .query_row(
            "SELECT value FROM namespace_metadata WHERE native_key = ?1 AND owner = ?2",
            params![namespace.as_str(), ticket.owner()],
            |row| row.get(0),
        )
        .optional()?;
    bytes
        .map(|bytes| serde_json::from_slice(&bytes).map_err(|_| Error::Unsupported))
        .transpose()
}

fn transition(
    host: &Host,
    context: &Context,
    ticket: &Ticket,
    expected: &Phase,
    next: &Phase,
) -> Result<(), Error> {
    context.recheck(host)?;
    let store = Store::mutate(host, false)?;
    store.connection.execute_batch("BEGIN IMMEDIATE")?;
    context.check_store(host, Some(&store))?;
    if read(&store, context, ticket)?.as_ref() != Some(expected) {
        return Err(Error::Conflict);
    }
    store.connection.execute(
        "UPDATE namespace_metadata SET value = ?3 WHERE native_key = ?1 AND owner = ?2",
        params![
            context.key().as_str(),
            ticket.owner(),
            serde_json::to_vec(next).map_err(|_| Error::Unsupported)?
        ],
    )?;
    store.connection.execute_batch("COMMIT")?;
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn execution_identity_rejects_runtime_names_and_short_ids() {
        for value in ["", "worker", "abc123", &"A".repeat(64), &"../x".repeat(16)] {
            assert!(Ticket::parse(value).is_err());
            assert!(Container::parse(value).is_err());
        }
        let id = "a".repeat(64);
        assert_eq!(Container::parse(&id).unwrap().as_str(), id);
    }

    #[test]
    fn execution_binding_roundtrips_exact_runtime_generation() {
        let phase = Phase::Bound(Container::parse(&"1".repeat(64)).unwrap());
        let bytes = serde_json::to_vec(&phase).unwrap();
        assert_eq!(serde_json::from_slice::<Phase>(&bytes).unwrap(), phase);
        assert!(serde_json::from_str::<Phase>(r#"{"bound":"abc"}"#).is_err());
        assert!(serde_json::from_str::<Phase>(r#"{"completed":true}"#).is_err());
    }
}
