//! Shared seed contracts checked against effective emitted image contents.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
    time::Duration,
};

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{Image, bounded};
use crate::artifact::text::Text;

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// shared image I/O failed: {0}
    Io(#[from] io::Error),
    /// invalid shared image metadata: {0}
    Json(#[from] serde_json::Error),
    /// shared image database query failed: {0}
    Sqlite(#[from] rusqlite::Error),
    /// shared image contract failed: {0}
    Contract(&'static str),
    /// emitted image permission verifier failed: {0}
    Permission(ExitStatus),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
enum Capability {
    One,
}

impl TryFrom<u64> for Capability {
    type Error = &'static str;
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::One),
            _ => Err("unsupported shared Nix capability"),
        }
    }
}

impl From<Capability> for u64 {
    fn from(value: Capability) -> Self {
        match value {
            Capability::One => 1,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Shared {
    version: Capability,
    seed_closure: PathBuf,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum SourceKind {
    NixDescriptor,
    DockerArchive,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Case {
    artifact: PathBuf,
    source: PathBuf,
    source_kind: SourceKind,
    shared: Shared,
    #[serde(default)]
    required_paths: Vec<String>,
    profile_config: Option<PathBuf>,
    digest_file: Option<PathBuf>,
}

const fn require(condition: bool, message: &'static str) -> Result<(), Error> {
    if condition {
        Ok(())
    } else {
        Err(Error::Contract(message))
    }
}

fn field<'a>(value: &'a Value, pointer: &str) -> Result<&'a Value, Error> {
    value
        .pointer(pointer)
        .ok_or(Error::Contract("missing image metadata field"))
}

fn read_json(path: &Path) -> Result<Value, Error> {
    let mut file = fs::File::open(path)?;
    let size = file.metadata()?.len();
    Ok(serde_json::from_slice(&bounded(&mut file, size)?)?)
}

fn seed(path: &Path) -> Result<BTreeSet<Text>, Error> {
    let contents = fs::read_to_string(path)?;
    let lines: Vec<_> = contents.lines().collect();
    let paths: BTreeSet<_> = lines.iter().map(|line| Text::from_utf8(line)).collect();
    require(
        !paths.is_empty() && paths.len() == lines.len(),
        "seed must be nonempty and unique",
    )?;
    Ok(paths)
}

fn materialized(image: &Image) -> Result<BTreeSet<Text>, Error> {
    let pattern = regex::bytes::Regex::new(r"(?-u)(?:^|/)nix/store/[a-z0-9]{32}-[^/]+")
        .map_err(io::Error::other)?;
    let mut candidates = BTreeSet::new();
    for name in image.names()? {
        if let Some(found) = pattern.find(&name.bytes()?) {
            let matched = found.as_bytes();
            let mut absolute = vec![b'/'];
            absolute.extend_from_slice(matched.strip_prefix(b"/").unwrap_or(matched));
            candidates.insert(Text::from_bytes(&absolute));
        }
    }
    let mut present = BTreeSet::new();
    for candidate in candidates {
        if image.lookup(&candidate)?.is_some() {
            present.insert(candidate);
        }
    }
    Ok(present)
}

fn extract(image: &Image, member: &str, destination: &Path) -> Result<(), Error> {
    require(
        image.extract(&Text::from_utf8(member), destination)?,
        "required image member absent",
    )
}

fn verify(case: &Case) -> Result<(), Error> {
    let image = Image::prepared(&case.artifact)?;
    let mut config_file = fs::File::open(case.artifact.join("config.json"))?;
    let config_size = config_file.metadata()?.len();
    let config_bytes = bounded(&mut config_file, config_size)?;
    let config: Value = serde_json::from_slice(&config_bytes)?;
    let labels = field(&config, "/config/Labels")?;
    require(
        field(labels, "/wrix.nix.private")? == "1",
        "private label differs",
    )?;
    require(
        field(labels, "/wrix.nix.shared")? == "1",
        "shared capability label differs",
    )?;
    let seed_name = case
        .shared
        .seed_closure
        .to_str()
        .ok_or(Error::Contract("non-UTF8 seed path"))?;
    require(
        field(labels, "/wrix.nix.seed")?
            == &Value::String(format!("{:x}", Sha256::digest(seed_name.as_bytes()))),
        "seed path commitment differs",
    )?;
    let mut environment = BTreeMap::new();
    for item in field(&config, "/config/Env")?
        .as_array()
        .ok_or(Error::Contract("environment is not an array"))?
    {
        let (key, value) = item
            .as_str()
            .and_then(|item| item.split_once('='))
            .ok_or(Error::Contract("invalid image environment item"))?;
        environment.insert(key, value);
    }
    require(
        environment.get("IS_SANDBOX") == Some(&"1"),
        "sandbox environment differs",
    )?;
    require(
        !environment.contains_key("NIX_REMOTE"),
        "remote Nix environment present",
    )?;
    for forbidden in ["nix/var/nix/daemon-socket/socket", "homeless-shelter"] {
        require(
            image.lookup(&Text::from_utf8(forbidden))?.is_none(),
            "forbidden effective image member present",
        )?;
    }
    let expected = seed(&case.shared.seed_closure)?;
    require(
        expected == materialized(&image)?,
        "seed differs from materialized store",
    )?;
    for required in &case.required_paths {
        require(
            expected.contains(&Text::from_utf8(required)),
            "required configuration path absent from seed",
        )?;
    }
    let temporary = tempfile::tempdir()?;
    let output = temporary.path().join("member");
    for marker in [
        "etc/wrix/private-nix-version",
        "etc/wrix/shared-nix-version",
    ] {
        extract(&image, marker, &output)?;
        require(fs::read(&output)? == b"1\n", "contract marker bytes differ")?;
    }
    let database = temporary.path().join("db.sqlite");
    extract(&image, "nix/var/nix/db/db.sqlite", &database)?;
    let connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    let registered = connection
        .prepare("SELECT path FROM ValidPaths")?
        .query_map([], |row| row.get::<_, String>(0))?
        .map(|row| row.map(|value| Text::from_utf8(&value)))
        .collect::<Result<BTreeSet<_>, _>>()?;
    require(
        expected == registered,
        "seed and private image registration disagree",
    )?;
    let shared = serde_json::to_value(&case.shared)?;
    if matches!(case.source_kind, SourceKind::NixDescriptor) {
        let descriptor = read_json(&case.source)?;
        require(
            field(&descriptor, "/shared_nix")? == &shared,
            "descriptor shared metadata differs",
        )?;
        require(
            field(&descriptor, "/config/labels")? == labels,
            "descriptor labels differ",
        )?;
        require(
            field(&descriptor, "/digest")?
                == &Value::String(format!("sha256:{:x}", Sha256::digest(&config_bytes))),
            "descriptor digest differs",
        )?;
    }
    if let Some(profile) = &case.profile_config {
        let profile = read_json(profile)?;
        require(
            field(&profile, "/image/shared_nix")? == &shared,
            "profile shared metadata differs",
        )?;
        require(
            field(&profile, "/image/source")? == &serde_json::to_value(&case.source)?,
            "profile source differs",
        )?;
        require(
            field(&profile, "/image/source_kind")? == &serde_json::to_value(case.source_kind)?,
            "profile source kind differs",
        )?;
        let digest = fs::read_to_string(
            case.digest_file
                .as_ref()
                .ok_or(Error::Contract("missing profile digest file"))?,
        )?;
        require(
            field(&profile, "/image/digest")? == digest.trim(),
            "profile digest differs",
        )?;
    }
    Ok(())
}

fn permission_command(case: &Case, program: &Path) -> Result<Command, Error> {
    let mut command = Command::new(program);
    match case.source_kind {
        SourceKind::DockerArchive => {
            command.arg("--archive").arg(&case.source);
        }
        SourceKind::NixDescriptor => {
            let descriptor = read_json(&case.source)?;
            let layout = field(&descriptor, "/oci_layout")?
                .as_str()
                .ok_or(Error::Contract("invalid descriptor layout path"))?;
            command.arg(layout);
        }
    }
    Ok(command)
}

pub fn run(cases_file: &Path, permission_program: &Path) -> Result<usize, Error> {
    let cases: Vec<Value> = serde_json::from_value(read_json(cases_file)?)?;
    require(!cases.is_empty(), "no shared image cases")?;
    for raw in &cases {
        let case: Case = serde_json::from_value(raw.clone())?;
        let status = permission_command(&case, permission_program)?.status()?;
        if !status.success() {
            return Err(Error::Permission(status));
        }
        verify(&case)?;
    }
    let temporary = tempfile::tempdir()?;
    let mut incomplete: Case = serde_json::from_value(cases[0].clone())?;
    let paths = fs::read_to_string(&incomplete.shared.seed_closure)?;
    let partial = temporary.path().join("store-paths");
    fs::write(
        &partial,
        format!("{}\n", paths.lines().skip(1).collect::<Vec<_>>().join("\n")),
    )?;
    incomplete.shared.seed_closure = partial;
    require(
        matches!(verify(&incomplete), Err(Error::Contract(_))),
        "image verifier accepted incomplete seed",
    )?;
    let mut incompatible = cases[0].clone();
    incompatible["shared"]["version"] = Value::from(2);
    require(
        serde_json::from_value::<Case>(incompatible).is_err(),
        "image verifier accepted incompatible capability",
    )?;
    Ok(cases.len())
}

#[cfg(test)]
mod test;
