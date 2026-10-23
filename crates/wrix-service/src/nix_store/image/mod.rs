//! Package-selected pool software is independent of project and core-service image selection.

use std::{env, ffi::OsString, path::Path};

use crate::lifecycle::binding;
use wrix_sandbox::image::{Digest, ImageRef, Source};

#[derive(Debug, displaydoc::Display, thiserror::Error)]
pub enum Error {
    /// the invoked host package has no dedicated Nix pool image binding; provision a compatible host Wrix package
    Missing,
    /// {0} cannot select Nix pool software; use the invoked host package's explicit binding
    Selector(&'static str),
    /// the package's Nix pool image binding failed: {0}
    Binding(#[from] binding::Error),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Candidate(binding::Candidate);

impl Candidate {
    pub fn from_env() -> Result<Self, Error> {
        let path = env::var_os("WRIX_NIX_POOL_PACKAGE_BINDING");
        Self::resolve(path.as_deref().map(Path::new), |name| env::var_os(name))
    }

    /// Parse only the selected package's metadata, without realization or runtime effects.
    pub fn resolve(
        path: Option<&Path>,
        lookup: impl Fn(&str) -> Option<OsString>,
    ) -> Result<Self, Error> {
        for name in [
            "WRIX_NIX_POOL_IMAGE",
            "WRIX_NIX_POOL_IMAGE_SOURCE",
            "WRIX_NIX_POOL_IMAGE_SOURCE_KIND",
            "WRIX_NIX_POOL_IMAGE_DIGEST",
        ] {
            if lookup(name).is_some() {
                return Err(Error::Selector(name));
            }
        }
        Ok(Self(binding::Candidate::load(path.ok_or(Error::Missing)?)?))
    }

    pub const fn image(&self) -> &ImageRef {
        self.0.image()
    }

    pub const fn source(&self) -> &Source {
        self.0.source()
    }

    /// Root the exact package-selected artifacts; this does not install or start a pool.
    pub fn realize(&self, roots: &Path) -> Result<Digest, Error> {
        Ok(self.0.realize(roots)?)
    }
}

#[cfg(test)]
mod test;
