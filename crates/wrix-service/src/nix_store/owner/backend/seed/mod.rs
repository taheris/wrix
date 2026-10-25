//! Exact installed worker-image inputs, not an arbitrary host Nix command or path-list grant.
use super::{COMMAND_BUDGET, Error, Program, Tools, success, text};
use crate::nix_store::native::image::{Id, StorePath};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Read,
    os::unix::fs::MetadataExt,
    path::Path,
};
use wrix_core::nix_store::{Image as Compatibility, pool::System};
use wrix_sandbox::image::Digest;

#[derive(Deserialize)]
enum Architecture {
    #[serde(rename = "amd64")]
    Amd64,
    #[serde(rename = "arm64")]
    Arm64,
    #[serde(other)]
    Unsupported,
}
#[derive(Deserialize)]
enum OperatingSystem {
    #[serde(rename = "linux")]
    Linux,
    #[serde(other)]
    Unsupported,
}
#[derive(Deserialize)]
struct Configuration {
    #[serde(rename = "Labels")]
    labels: BTreeMap<String, String>,
}
#[derive(Deserialize)]
struct Observation {
    #[serde(rename = "Id", deserialize_with = "config_id")]
    id: Id,
    #[serde(rename = "Architecture")]
    architecture: Architecture,
    #[serde(rename = "Os")]
    os: OperatingSystem,
    #[serde(rename = "Config")]
    configuration: Configuration,
}

fn config_id<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Id, D::Error> {
    let value = String::deserialize(deserializer)?;
    Id::try_from(
        value
            .strip_prefix("sha256:")
            .unwrap_or(&value)
            .to_ascii_lowercase(),
    )
    .map_err(serde::de::Error::custom)
}

/// Immutable native image binding checked against its generated seed metadata, not a wire capability.
pub struct Image {
    id: Id,
    system: System,
    closure: StorePath,
    paths: Vec<StorePath>,
}
impl Image {
    pub fn inspect(
        system: System,
        digest: &Digest,
        compatibility: &Compatibility,
    ) -> Result<Self, Error> {
        let _host =
            wrix_state::host::Host::discover().map_err(crate::nix_store::owner::Error::from)?;
        compatibility
            .version
            .require_supported()
            .map_err(|_| Error::Invalid)?;
        let tools = Tools::from_env()?;
        let mut command = tools.command(Program::Podman);
        command.args(["image", "inspect", digest.as_str()]);
        let mut observations: Vec<Observation> =
            serde_json::from_slice(&success(tools.run(command, None, Some(COMMAND_BUDGET))?)?)?;
        if observations.len() != 1 {
            return Err(Error::Invalid);
        }
        let observation = observations.pop().ok_or(Error::Invalid)?;
        let id = Self::binding(system, digest, compatibility, &observation)?;
        let path = compatibility.seed_closure.as_path();
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file()
            || metadata.mode() & 0o222 != 0
            || ![0, rustix::process::geteuid().as_raw()].contains(&metadata.uid())
            || path.canonicalize()? != path
            || metadata.len() > 2_097_152
        {
            return Err(Error::Invalid);
        }
        let mut bytes = Vec::new();
        File::open(path)?.take(2_097_153).read_to_end(&mut bytes)?;
        if bytes.len() > 2_097_152 {
            return Err(Error::Invalid);
        }
        let paths = Self::paths(&bytes)?;
        let closure = StorePath::try_from(
            path.parent()
                .and_then(Path::to_str)
                .ok_or(Error::Invalid)?
                .to_owned(),
        )?;
        Ok(Self {
            id,
            system,
            closure,
            paths,
        })
    }
    fn binding(
        system: System,
        expected: &Digest,
        compatibility: &Compatibility,
        observed: &Observation,
    ) -> Result<Id, Error> {
        let expected_id = expected
            .as_str()
            .strip_prefix("sha256:")
            .ok_or(Error::Invalid)?
            .to_ascii_lowercase();
        if observed.id.as_str() != expected_id
            || !matches!(observed.os, OperatingSystem::Linux)
            || !matches!(
                (system, &observed.architecture),
                (System::X86_64Linux, Architecture::Amd64)
                    | (System::Aarch64Linux, Architecture::Arm64)
            )
        {
            return Err(Error::Invalid);
        }
        let hash = format!(
            "{:x}",
            Sha256::digest(
                compatibility
                    .seed_closure
                    .as_path()
                    .as_os_str()
                    .as_encoded_bytes()
            )
        );
        for (label, value) in [
            ("wrix.managed", "true"),
            ("wrix.image.kind", "profile"),
            ("wrix.nix.private", "1"),
            ("wrix.nix.shared", "1"),
            ("wrix.nix.client", "1"),
            ("wrix.nix.seed", hash.as_str()),
        ] {
            if observed.configuration.labels.get(label).map(String::as_str) != Some(value) {
                return Err(Error::Invalid);
            }
        }
        Ok(Id::try_from(expected_id)?)
    }
    fn paths(bytes: &[u8]) -> Result<Vec<StorePath>, Error> {
        let value = std::str::from_utf8(bytes).map_err(|_| Error::Invalid)?;
        let mut paths = BTreeSet::new();
        let mut count = 0;
        for line in value.lines() {
            if count >= 4096 {
                return Err(Error::Invalid);
            }
            if !paths.insert(StorePath::try_from(line.to_owned())?) {
                return Err(Error::Invalid);
            }
            count += 1;
        }
        if count == 0 || !value.ends_with('\n') {
            return Err(Error::Invalid);
        }
        Ok(paths.into_iter().collect())
    }
    pub const fn id(&self) -> &Id {
        &self.id
    }
    pub fn closure_paths(&self) -> &[StorePath] {
        &self.paths
    }
    fn budget(tools: &Tools, paths: &[StorePath]) -> Result<u64, Error> {
        let mut command = tools.command(Program::NixStore);
        command
            .args(["--query", "--size"])
            .args(paths.iter().map(StorePath::as_str));
        let output = text(success(tools.run(command, None, Some(COMMAND_BUDGET))?)?)?;
        let mut total = 4_194_304_u64;
        let mut count = 0;
        for line in output.lines() {
            total = total
                .checked_add(line.parse::<u64>().map_err(|_| Error::Invalid)?)
                .ok_or(Error::Invalid)?;
            count += 1;
        }
        if count != paths.len() || total > 8_589_934_592 {
            return Err(Error::Invalid);
        }
        Ok(total)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Imported,
    Linked,
    Reused,
}

impl super::Owner {
    /// Copies only a verified installed image closure under original checked issuance, never worker runtime credentials.
    pub fn seed_execution(
        &self,
        issued: &wrix_state::nix_store::admission::request::Accepted,
        image: &Image,
    ) -> Result<Outcome, Error> {
        use super::{check_directory, native, private_file};
        use fs2::FileExt;
        use std::os::unix::fs::DirBuilderExt;
        issued.recheck()?;
        if issued.pool() != self.pool()
            || image.system != self.pool().system
            || !matches!(
                issued.purpose(),
                wrix_state::nix_store::admission::request::Purpose::Execution { .. }
                    | wrix_state::nix_store::admission::request::Purpose::Worker { .. }
            )
        {
            return Err(Error::Invalid);
        }
        if let wrix_state::nix_store::admission::request::Purpose::Worker { image: binding, .. } =
            issued.purpose()
            && (binding.digest().as_str().strip_prefix("sha256:") != Some(image.id.as_str())
                || binding.metadata().seed_closure.as_path().parent()
                    != Some(Path::new(image.closure.as_str())))
        {
            return Err(Error::Invalid);
        }
        if !matches!(self.record.state, super::State::Serving { .. }) {
            return Err(Error::Retained);
        }
        self.inspect()?;
        let connection = self.connection()?;
        if let wrix_state::nix_store::admission::request::Purpose::Worker { plane, .. } =
            issued.purpose()
        {
            connection
                .target
                .attempt
                .software
                .tools
                .view_check(*plane)?;
        }
        match self.rpc(
            connection,
            &native::Operation::InspectImage {
                image: image.id.clone(),
                paths: image.paths.clone(),
            },
            Some(COMMAND_BUDGET),
        )? {
            native::Reply::Image {
                status: native::ImageStatus::Pinned,
            } => return Ok(Outcome::Reused),
            native::Reply::Image {
                status: native::ImageStatus::Missing,
            } => (),
            native::Reply::Busy => return Err(Error::Busy),
            _ => return Err(Error::Retained),
        }
        let imports = match self.rpc(
            connection,
            &native::Operation::InspectImageMissing {
                image: image.id.clone(),
                paths: image.paths.clone(),
            },
            Some(COMMAND_BUDGET),
        )? {
            native::Reply::MissingPaths { paths } => paths,
            native::Reply::Busy => return Err(Error::Busy),
            _ => return Err(Error::Retained),
        };
        let imports_set: BTreeSet<_> = imports.iter().cloned().collect();
        if imports_set.len() != imports.len()
            || !imports.iter().all(|path| image.paths.contains(path))
        {
            return Err(Error::Invalid);
        }
        if imports.is_empty() {
            if !matches!(
                self.rpc(
                    connection,
                    &native::Operation::SeedImage {
                        image: image.id.clone(),
                        paths: image.paths.clone(),
                        imports
                    },
                    None
                )?,
                native::Reply::Applied
            ) {
                return Err(Error::Retained);
            }
            if !matches!(
                self.rpc(
                    connection,
                    &native::Operation::InspectImage {
                        image: image.id.clone(),
                        paths: image.paths.clone()
                    },
                    Some(COMMAND_BUDGET)
                )?,
                native::Reply::Image {
                    status: native::ImageStatus::Pinned
                }
            ) {
                return Err(Error::Retained);
            }
            return Ok(Outcome::Linked);
        }
        let tools = &connection.target.attempt.software.tools;
        let control = &self.prepared.reservation.paths.control;
        let roots = control.join("seed-roots");
        match fs::DirBuilder::new().mode(0o700).create(&roots) {
            Ok(()) => File::open(control)?.sync_all()?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                check_directory(&roots)?;
            }
            Err(error) => return Err(error.into()),
        }
        let root = roots.join(image.id.as_str());
        fs::DirBuilder::new().mode(0o700).create(&root)?;
        File::open(&roots)?.sync_all()?;
        let input = root.join("input");
        let mut command = tools.command(Program::NixStore);
        command
            .args(["--realise", image.closure.as_str(), "--add-root"])
            .arg(&input)
            .arg("--indirect");
        success(tools.run(command, None, None)?)?;
        let mut command = tools.command(Program::NixStore);
        command.args(["--query", "--requisites", image.closure.as_str()]);
        let closure = Image::paths(&success(tools.run(command, None, Some(COMMAND_BUDGET))?)?)?;
        let closure: BTreeSet<_> = closure.into_iter().collect();
        if !image.paths.iter().all(|path| closure.contains(path)) {
            return Err(Error::Invalid);
        }
        let budget = Image::budget(tools, &imports)?;
        let incoming = self.prepared.data_root().join("incoming");
        check_directory(&incoming)?;
        if fs2::available_space(&incoming)?
            < budget.checked_add(1_073_741_824).ok_or(Error::Invalid)?
        {
            return Err(crate::nix_store::owner::Error::Space.into());
        }
        let name = format!("{}.nar", image.id.as_str());
        let archive = private_file(&incoming, &name, true)?;
        FileExt::allocate(&archive, budget)?;
        if archive.metadata()?.len() != budget || FileExt::allocated_size(&archive)? < budget {
            return Err(Error::Invalid);
        }
        let (status, size) = tools.export(&imports, &archive, budget)?;
        if !status.success() {
            return Err(Error::Helper(status.code()));
        }
        if size == 0 {
            return Err(Error::Invalid);
        }
        archive.set_len(size)?;
        archive.sync_all()?;
        File::open(&incoming)?.sync_all()?;
        issued.recheck()?;
        if !matches!(
            self.rpc(
                connection,
                &native::Operation::SeedImage {
                    image: image.id.clone(),
                    paths: image.paths.clone(),
                    imports,
                },
                None
            )?,
            native::Reply::Applied
        ) {
            return Err(Error::Retained);
        }
        if !matches!(
            self.rpc(
                connection,
                &native::Operation::InspectImage {
                    image: image.id.clone(),
                    paths: image.paths.clone()
                },
                Some(COMMAND_BUDGET)
            )?,
            native::Reply::Image {
                status: native::ImageStatus::Pinned
            }
        ) {
            return Err(Error::Retained);
        }
        let current = fs::symlink_metadata(incoming.join(&name))?;
        let expected = archive.metadata()?;
        if !current.is_file()
            || current.dev() != expected.dev()
            || current.ino() != expected.ino()
            || current.nlink() != 1
        {
            return Err(Error::Invalid);
        }
        fs::remove_file(incoming.join(name))?;
        File::open(&incoming)?.sync_all()?;
        if fs::read_link(&input)? != Path::new(image.closure.as_str()) {
            return Err(Error::Invalid);
        }
        fs::remove_file(input)?;
        File::open(&root)?.sync_all()?;
        fs::remove_dir(root)?;
        File::open(roots)?.sync_all()?;
        Ok(Outcome::Imported)
    }
}

#[cfg(test)]
mod test;
