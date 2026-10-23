//! Host-private frozen observations for one-use issuance. Never accept these from an IPC peer.
use super::Context;
use crate::{
    host::Host,
    store::{Error, Store},
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::PathBuf,
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    caller: Vec<u8>,
    selection: serde_json::Value,
    observation: serde_json::Value,
}

fn selection(value: Option<&(crate::integration::Slot, PathBuf)>) -> serde_json::Value {
    serde_json::json!(
        value.map(|(slot, executable)| (slot.as_str(), crate::resolver::encode(executable)))
    )
}

fn observation(context: &Context) -> serde_json::Value {
    serde_json::json!({
        "caller": crate::resolver::encode(&context.caller),
        "primary": crate::resolver::encode(&context.primary),
        "namespace": context.key.as_str(),
        "directory": context.directory,
        "checkout": context.checkout.as_ref().map(crate::resolver::Checkout::snapshot),
        "resolution": context.resolution.as_ref().map(|resolution| serde_json::json!({
            "caller": crate::resolver::encode(resolution.caller()),
            "membership": resolution.membership().as_str(),
            "checkout": resolution.checkout().snapshot(),
            "primary": resolution.primary().snapshot(),
        })),
    })
}

impl Snapshot {
    pub(crate) fn capture(context: &Context) -> Self {
        Self {
            caller: context.caller.as_os_str().as_bytes().to_vec(),
            selection: selection(context.selection.as_ref()),
            observation: observation(context),
        }
    }

    /// Re-observe only the original caller; compare every frozen observation, not a newly selected hash/path.
    pub(crate) fn recheck(&self, host: &Host) -> Result<Context, Error> {
        let store = Store::inspect(host)?.ok_or(Error::Uninitialized)?;
        if selection(store.integration()?.as_ref()) != self.selection {
            return Err(Error::Conflict);
        }
        let caller = PathBuf::from(OsString::from_vec(self.caller.clone()));
        let context = Context::inspect(host, &caller)?;
        if observation(&context) != self.observation {
            return Err(Error::Conflict);
        }
        context.recheck(host)?;
        Ok(context)
    }
}
