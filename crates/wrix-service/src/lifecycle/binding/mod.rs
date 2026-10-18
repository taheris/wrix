use std::{
    env, fs, io,
    path::{Component, Path, PathBuf},
    process::Command,
};

use displaydoc::Display;
use serde::Deserialize;
use thiserror::Error;
use wrix_sandbox::image::{Digest, ImageRef, Source, SourceKind};

#[derive(Debug, Display, Error)]
pub enum Error {
    /// the invoked host Wrix package has no service-image binding; provision a bound host Wrix package
    Missing,
    /// service-image binding I/O failed: {0}
    Io(#[from] io::Error),
    /// invalid service-image binding: {0}
    Json(#[from] serde_json::Error),
    /// invalid service-image binding metadata: {0}
    Metadata(&'static str),
    /// environment variable {0} conflicts with the invoked host package's service-image binding
    Conflict(&'static str),
    /// service-image derivation operation failed: {0}
    Derivation(String),
    /// invalid bound service-image digest: {0}
    Digest(String),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct Artifact {
    path: PathBuf,
    derivation: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    schema: u8,
    image: ImageRef,
    source_kind: SourceKind,
    source: Artifact,
    digest: Artifact,
}

/// An immutable package-selected image target, independent of executable and project paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Candidate {
    image: ImageRef,
    source: Source,
    source_artifact: Artifact,
    digest_artifact: Artifact,
}

impl Candidate {
    /// Parse package metadata without realizing sources or contacting a runtime.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let wire: Wire = serde_json::from_slice(&fs::read(path)?)?;
        if wire.schema != 1 {
            return Err(Error::Metadata("unsupported schema"));
        }
        wire.source.check()?;
        wire.digest.check()?;
        let source = Source::parse(wire.source.path.clone(), wire.source_kind)
            .map_err(|_| Error::Metadata("invalid source path"))?;
        Ok(Self {
            image: wire.image,
            source,
            source_artifact: wire.source,
            digest_artifact: wire.digest,
        })
    }

    /// Resolve only the invoked package's explicit binding; ambient data cannot supply it.
    pub fn from_env() -> Result<Self, Error> {
        let path = env::var_os("WRIX_SERVICE_PACKAGE_BINDING").map(PathBuf::from);
        Self::resolve(path.as_deref(), |name| env::var_os(name))
    }

    /// Resolve a package binding with an explicit ambient-input boundary.
    pub fn resolve(
        path: Option<&Path>,
        lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> Result<Self, Error> {
        let candidate = Self::load(path.ok_or(Error::Missing)?)?;
        candidate.check_environment(lookup)?;
        Ok(candidate)
    }

    fn check_environment(
        &self,
        lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> Result<(), Error> {
        for (name, expected) in [
            ("WRIX_SERVICE_IMAGE", self.image.as_str().as_ref()),
            ("WRIX_SERVICE_IMAGE_SOURCE", self.source.path().as_os_str()),
            (
                "WRIX_SERVICE_IMAGE_SOURCE_KIND",
                self.source.kind().as_str().as_ref(),
            ),
            (
                "WRIX_SERVICE_IMAGE_DIGEST",
                self.digest_artifact.path.as_os_str(),
            ),
        ] {
            if lookup(name).is_some_and(|value| value != expected) {
                return Err(Error::Conflict(name));
            }
        }
        Ok(())
    }

    pub const fn image(&self) -> &ImageRef {
        &self.image
    }

    pub const fn source(&self) -> &Source {
        &self.source
    }

    /// Realize and root the selected artifacts, never install a runtime image.
    pub fn realize(&self, roots: &Path) -> Result<Digest, Error> {
        let source_output = self.source_artifact.output()?;
        let digest_output = self.digest_artifact.output()?;
        self.source_artifact.realize(roots, &source_output)?;
        self.digest_artifact.realize(roots, &digest_output)?;
        let value = fs::read_to_string(&self.digest_artifact.path)?;
        Digest::parse(value.trim()).map_err(|_| Error::Digest(value))
    }
}

impl Artifact {
    fn check(&self) -> Result<(), Error> {
        for path in [&self.path, &self.derivation] {
            if !path.is_absolute()
                || path
                    .components()
                    .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
                || path.as_os_str().as_encoded_bytes().contains(&0)
            {
                return Err(Error::Metadata(
                    "artifact paths must be absolute and normalized",
                ));
            }
        }
        if self
            .derivation
            .extension()
            .is_none_or(|extension| extension != "drv")
        {
            return Err(Error::Metadata("artifact derivation must name a .drv file"));
        }
        Ok(())
    }

    fn output(&self) -> Result<PathBuf, Error> {
        let output = Command::new("nix-store")
            .args(["--query", "--outputs"])
            .arg(&self.derivation)
            .output()?;
        if !output.status.success() {
            return Err(Error::Derivation(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        let outputs = String::from_utf8(output.stdout)
            .map_err(|_| Error::Metadata("non-Unicode derivation outputs"))?;
        let mut outputs = outputs.lines();
        let output = outputs
            .next()
            .ok_or(Error::Metadata("missing derivation output"))?;
        if outputs.next().is_some()
            || !Path::new(output).is_absolute()
            || !self.path.starts_with(output)
        {
            return Err(Error::Metadata(
                "artifact is not the sole output of its bound derivation",
            ));
        }
        Ok(PathBuf::from(output))
    }

    fn realize(&self, roots: &Path, output: &Path) -> Result<(), Error> {
        let name = output
            .file_name()
            .ok_or(Error::Metadata("missing output basename"))?;
        fs::create_dir_all(roots)?;
        let output = Command::new("nix-store")
            .arg("--realise")
            .arg(&self.derivation)
            .arg("--add-root")
            .arg(roots.join(name))
            .arg("--indirect")
            .output()?;
        if !output.status.success() {
            return Err(Error::Derivation(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        Ok(())
    }
}
