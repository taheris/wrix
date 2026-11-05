use std::{
    env, fs, io,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

use displaydoc::Display;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use wrix_sandbox::image::{Digest, ImageRef, Source, SourceKind};

pub mod resources;

#[cfg(test)]
mod test;

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

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct Artifact {
    path: PathBuf,
    derivation: PathBuf,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    schema: u8,
    image: ImageRef,
    source_kind: SourceKind,
    source: Artifact,
    digest: Artifact,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resources: Option<resources::Resources>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Artifacts {
    source: Artifact,
    digest: Artifact,
}

/// An immutable package-selected image target, independent of executable and project paths.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(try_from = "Wire")]
pub struct Candidate {
    image: ImageRef,
    source: Source,
    artifacts: Box<Artifacts>,
    resources: Option<resources::Resources>,
}

impl Candidate {
    /// Parse package metadata without realizing sources or contacting a runtime.
    pub fn load(path: &Path) -> Result<Self, Error> {
        Ok(serde_json::from_slice(&fs::read(path)?)?)
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
                self.artifacts.digest.path.as_os_str(),
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

    /// VM limits belong to the selected package, including during recovery.
    pub const fn resources(&self) -> Option<&resources::Resources> {
        self.resources.as_ref()
    }

    /// Realize and root the selected artifacts, never install a runtime image.
    pub fn realize(&self, roots: &Path) -> Result<Digest, Error> {
        let source_output = self.artifacts.source.output()?;
        let digest_output = self.artifacts.digest.output()?;
        self.artifacts.source.realize(roots, &source_output)?;
        self.artifacts.digest.realize(roots, &digest_output)?;
        let value = fs::read_to_string(&self.artifacts.digest.path)?;
        Digest::parse(value.trim()).map_err(|_| Error::Digest(value))
    }
}

impl TryFrom<Wire> for Candidate {
    type Error = Error;

    fn try_from(wire: Wire) -> Result<Self, Error> {
        if !matches!(
            (wire.schema, wire.resources.is_some()),
            (1, false) | (2, true)
        ) {
            return Err(Error::Metadata("unsupported schema or resource policy"));
        }
        wire.source.check()?;
        wire.digest.check()?;
        let source = Source::parse(wire.source.path.clone(), wire.source_kind)
            .map_err(|_| Error::Metadata("invalid source path"))?;
        Ok(Self {
            image: wire.image,
            source,
            artifacts: Box::new(Artifacts {
                source: wire.source,
                digest: wire.digest,
            }),
            resources: wire.resources,
        })
    }
}

impl Serialize for Candidate {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        Wire {
            schema: if self.resources.is_some() { 2 } else { 1 },
            image: self.image.clone(),
            source_kind: self.source.kind(),
            source: self.artifacts.source.clone(),
            digest: self.artifacts.digest.clone(),
            resources: self.resources.clone(),
        }
        .serialize(serializer)
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
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .status()?;
        if !output.success() {
            return Err(Error::Derivation(format!(
                "{} exited with {output}; see Nix output above",
                self.derivation.display()
            )));
        }
        Ok(())
    }
}
