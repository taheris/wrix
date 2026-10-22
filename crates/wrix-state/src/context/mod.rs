//! Checked namespace selection; caller location remains separate from shared service scope.

use crate::{
    host::Host,
    resolver::{Checkout, Resolution, resolve},
    store::{Error, Store},
};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
use wrix_core::path::{Workspace, WorkspaceHash};

#[derive(Clone, Debug)]
pub struct Context {
    caller: PathBuf,
    primary: PathBuf,
    key: WorkspaceHash,
    resolution: Option<Resolution>,
    checkout: Option<Checkout>,
    directory: (u64, u64),
    generation: Option<crate::store::Generation>,
    selection: Option<(crate::integration::Slot, PathBuf)>,
}

impl Context {
    pub fn check_mount(&self, source: &Path) -> Result<(), Error> {
        if let Some((_, executable)) = &self.selection {
            let source = source.canonicalize()?;
            let executable = executable.canonicalize()?;
            if executable.starts_with(&source) {
                return Err(Error::Invalid(
                    "mount would expose the host-selected workspace resolver".into(),
                ));
            }
        }
        Ok(())
    }
    pub fn inspect(host: &Host, caller: &Path) -> Result<Self, Error> {
        let caller = caller.canonicalize()?;
        if !caller.is_dir() {
            return Err(Error::Invalid("caller must be a directory".into()));
        }
        let store = Store::inspect(host)?;
        let generation = store.as_ref().map(Store::generation).transpose()?;
        let selection = store
            .as_ref()
            .map(Store::integration)
            .transpose()?
            .flatten();
        let (primary, key, resolution, checkout) = if let Some((slot, executable)) = &selection {
            let store = store.as_ref().ok_or(Error::Uninitialized)?;
            if !store.initialized()? {
                return Err(Error::Uninitialized);
            }
            let resolution = resolve(host, executable, &caller)?;
            let key = store.admitted(host, slot, &resolution)?;
            (
                resolution.primary().root().to_path_buf(),
                key,
                Some(resolution),
                None,
            )
        } else {
            let workspace = Workspace::from_service_path(&caller)?;
            if let Some(store) = store {
                store.standalone(workspace.hash(), false)?;
            }
            let checkout = if workspace.repository_root().is_some() {
                Some(Checkout::discover(&caller)?)
            } else {
                None
            };
            let primary = checkout.as_ref().map_or_else(
                || workspace.canonical_path().to_path_buf(),
                |checkout| checkout.root().to_path_buf(),
            );
            let key = Workspace::from_path(&primary)?.hash().clone();
            (primary, key, None, checkout)
        };
        let metadata = fs::metadata(&primary)?;
        let context = Self {
            caller,
            primary,
            key,
            resolution,
            checkout,
            directory: (metadata.dev(), metadata.ino()),
            generation,
            selection,
        };
        let current = Store::inspect(host)?;
        context.check_store(host, current.as_ref())?;
        Ok(context)
    }

    pub(crate) fn primary_checkout(&self) -> Option<&Checkout> {
        self.resolution
            .as_ref()
            .map(crate::resolver::Resolution::primary)
            .or(self.checkout.as_ref())
    }

    pub fn caller(&self) -> &Path {
        &self.caller
    }
    pub fn primary(&self) -> &Path {
        &self.primary
    }
    pub const fn key(&self) -> &WorkspaceHash {
        &self.key
    }
    pub const fn integrated(&self) -> bool {
        self.resolution.is_some()
    }

    pub fn workspace(&self) -> Result<Workspace, Error> {
        let workspace = Workspace::from_path(&self.primary)?.in_namespace(self.key.clone());
        if workspace.canonical_path().as_os_str() != self.primary.as_os_str() {
            return Err(Error::Conflict);
        }
        Ok(workspace)
    }

    pub fn recheck(&self, host: &Host) -> Result<(), Error> {
        let store = Store::inspect(host)?;
        self.check_store(host, store.as_ref())?;
        if let Some(resolution) = &self.resolution {
            let executable = &self.selection.as_ref().ok_or(Error::Conflict)?.1;
            if !resolution.matches(&resolve(host, executable, &self.caller)?) {
                return Err(Error::Conflict);
            }
        } else if let Some(checkout) = &self.checkout {
            if !checkout.matches(&Checkout::discover(&self.caller)?) {
                return Err(Error::Conflict);
            }
        } else if Workspace::from_service_path(&self.caller)?
            .canonical_path()
            .as_os_str()
            != self.primary.as_os_str()
        {
            return Err(Error::Conflict);
        }
        let current = Store::inspect(host)?;
        self.check_store(host, current.as_ref())
    }

    pub(crate) fn check_store(&self, host: &Host, store: Option<&Store>) -> Result<(), Error> {
        if store.map(Store::generation).transpose()? != self.generation
            || store.map(Store::integration).transpose()?.flatten() != self.selection
        {
            return Err(Error::Conflict);
        }
        let metadata = fs::metadata(&self.primary)?;
        if (metadata.dev(), metadata.ino()) != self.directory
            || self.caller.canonicalize()? != self.caller
        {
            return Err(Error::Conflict);
        }
        if let Some(resolution) = &self.resolution {
            let store = store.ok_or(Error::Uninitialized)?;
            let slot = &self.selection.as_ref().ok_or(Error::Conflict)?.0;
            if store.admitted(host, slot, resolution)? != self.key {
                return Err(Error::Conflict);
            }
        } else {
            if let Some(checkout) = &self.checkout {
                checkout.recheck()?;
            }
            if let Some(store) = store {
                store.standalone(&self.key, false)?;
                crate::namespace::scope::check(store, &self.key, &self.primary)?;
            }
        }
        Ok(())
    }

    pub fn authorize_effects(&self, host: &Host) -> Result<(), Error> {
        self.recheck(host)?;
        if !self.integrated() && !host.roots().workspace_state(&self.key).try_exists()? {
            Store::inspect(host)?
                .ok_or(Error::Acknowledgment)?
                .standalone(&self.key, true)?;
        }
        crate::namespace::authorize(host, self)
    }
}

/// Host operations retain their selection for rechecks; workers use only their own namespace.
#[derive(Clone, Debug)]
pub struct Selection {
    workspace: Workspace,
    context: Option<Context>,
}

impl Selection {
    pub fn inspect(caller: &Path) -> Result<Self, Error> {
        match Host::discover() {
            Ok(host) => {
                let context = Context::inspect(&host, caller)?;
                let workspace = context.workspace()?;
                Ok(Self {
                    workspace,
                    context: Some(context),
                })
            }
            Err(crate::host::Error::Worker) => Ok(Self {
                workspace: Workspace::from_service_path(caller)?,
                context: None,
            }),
            Err(error) => Err(error.into()),
        }
    }

    pub const fn workspace(&self) -> &Workspace {
        &self.workspace
    }
    pub const fn context(&self) -> Option<&Context> {
        self.context.as_ref()
    }

    pub fn recheck(&self) -> Result<(), Error> {
        match (&self.context, Host::discover()) {
            (Some(context), Ok(host)) => context.recheck(&host),
            (None, Err(crate::host::Error::Worker)) => Ok(()),
            (_, Err(error)) => Err(error.into()),
            (None, Ok(_)) => Err(Error::Conflict),
        }
    }

    pub fn authorize_effects(&self) -> Result<(), Error> {
        match (&self.context, Host::discover()) {
            (Some(context), Ok(host)) => context.authorize_effects(&host),
            (None, Err(crate::host::Error::Worker)) => Ok(()),
            (_, Err(error)) => Err(error.into()),
            (None, Ok(_)) => Err(Error::Conflict),
        }
    }
}

pub fn service_workspace(caller: &Path, effects: bool) -> Result<Workspace, Error> {
    let selection = Selection::inspect(caller)?;
    if effects {
        selection.authorize_effects()?;
    }
    Ok(selection.workspace)
}
