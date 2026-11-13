use super::Error;
use crate::native_fixture::root;
use std::{fs, path::Path};

#[derive(Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Nonce(String);
impl Clone for Nonce {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl TryFrom<String> for Nonce {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("invalid native interruption nonce");
        }
        Ok(Self(value))
    }
}
impl From<Nonce> for String {
    fn from(value: Nonce) -> Self {
        value.0
    }
}
impl Nonce {
    pub fn random() -> Result<Self, Error> {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes)?;
        Ok(Self(
            bytes
                .iter()
                .flat_map(|byte| {
                    let hex = b"0123456789abcdef";
                    [
                        char::from(hex[usize::from(byte >> 4)]),
                        char::from(hex[usize::from(byte & 15)]),
                    ]
                })
                .collect(),
        ))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Controller,
    WorkerImage,
    FirstWorker,
    SecondWorker,
    WorkerEvaluations,
    WorkersReady,
}
impl Phase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Controller => "controller",
            Self::WorkerImage => "worker-image",
            Self::FirstWorker => "first-worker",
            Self::SecondWorker => "second-worker",
            Self::WorkerEvaluations => "worker-evaluations",
            Self::WorkersReady => "workers-ready",
        }
    }
}
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    version: u8,
    phase: Phase,
    nonce: Nonce,
    device: u64,
    inode: u64,
}

pub fn observe(
    registry: &Path,
    nonce: &Nonce,
    mut report: impl FnMut(&Path, Phase) -> Result<(), Error>,
) -> Result<Option<root::Root>, Error> {
    use std::os::unix::fs::MetadataExt;
    let entries = root::registry(registry)?;
    if entries.len() > 1 {
        return Err(Error::Policy(
            "interruption requires exactly one disposable fixture",
        ));
    }
    for path in entries.keys() {
        let selected = root::Root::parse(path, root::Origin::Registered)?;
        for (filename, ready) in [
            (".wrix-native-test-progress.json", false),
            (".wrix-native-test-ready.json", true),
        ] {
            let file = match root::open_private(&selected.child(filename)?, 4096) {
                Ok(file) => file,
                Err(root::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    continue;
                }
                Err(_) => {
                    return Err(Error::Policy(if ready {
                        "unsafe native readiness checkpoint"
                    } else {
                        "unsafe native progress checkpoint"
                    }));
                }
            };
            let record: Checkpoint = serde_json::from_reader(file).map_err(|_| {
                Error::Policy(if ready {
                    "foreign or malformed native readiness checkpoint"
                } else {
                    "foreign or malformed native progress checkpoint"
                })
            })?;
            selected.recheck()?;
            let identity = fs::metadata(selected.path())?;
            if record.version != 1
                || (record.phase == Phase::WorkersReady) != ready
                || record.nonce != *nonce
                || (record.device, record.inode) != (identity.dev(), identity.ino())
            {
                return Err(Error::Policy(if ready {
                    "foreign or malformed native readiness checkpoint"
                } else {
                    "foreign or malformed native progress checkpoint"
                }));
            }
            if ready {
                return Ok(Some(selected));
            }
            report(selected.path(), record.phase)?;
        }
    }
    Ok(None)
}

#[cfg(test)]
mod test {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
    fn setup() -> (tempfile::TempDir, tempfile::TempDir, Nonce) {
        let root = tempfile::tempdir().unwrap();
        let registry = tempfile::tempdir().unwrap();
        fs::set_permissions(registry.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(
            registry.path().join("root"),
            root.path().as_os_str().as_encoded_bytes(),
        )
        .unwrap();
        (root, registry, Nonce::try_from("a".repeat(64)).unwrap())
    }
    fn record(root: &Path, nonce: &Nonce, phase: &str) -> serde_json::Value {
        let metadata = fs::metadata(root).unwrap();
        serde_json::json!({"version":1,"phase":phase,"nonce":nonce,"device":metadata.dev(),"inode":metadata.ino()})
    }
    fn store(path: &Path, value: &serde_json::Value) {
        fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    #[test]
    fn exact_readiness_requires_private_identity_bound_record() {
        let (root, registry, nonce) = setup();
        let path = root.path().join(".wrix-native-test-ready.json");
        store(&path, &record(root.path(), &nonce, "workers-ready"));
        assert_eq!(
            observe(registry.path(), &nonce, |_, _| panic!(
                "readiness is not progress"
            ))
            .unwrap()
            .unwrap()
            .path(),
            root.path()
        );
    }
    #[test]
    fn progress_reports_setup_without_claiming_readiness() {
        let (root, registry, nonce) = setup();
        store(
            &root.path().join(".wrix-native-test-progress.json"),
            &record(root.path(), &nonce, "worker-image"),
        );
        let mut phases = vec![];
        assert!(
            observe(registry.path(), &nonce, |_, phase| {
                phases.push(phase);
                Ok(())
            })
            .unwrap()
            .is_none()
        );
        assert_eq!(phases, vec![Phase::WorkerImage]);
    }
    #[test]
    fn foreign_nonce_identity_unknown_phase_and_extra_fields_are_rejected() {
        let (root, registry, nonce) = setup();
        let path = root.path().join(".wrix-native-test-ready.json");
        let original = record(root.path(), &nonce, "workers-ready");
        for (key, value) in [
            ("nonce", serde_json::json!("e".repeat(64))),
            ("inode", serde_json::json!(0)),
            ("phase", serde_json::json!("worker-image")),
            ("extra", serde_json::json!(true)),
        ] {
            let mut changed = original.clone();
            changed[key] = value;
            store(&path, &changed);
            assert!(observe(registry.path(), &nonce, |_, _| Ok(())).is_err());
        }
    }
    #[test]
    fn symlink_public_hardlinked_and_oversized_readiness_are_not_authority() {
        let (root, registry, nonce) = setup();
        let path = root.path().join(".wrix-native-test-ready.json");
        let peer = root.path().join("peer");
        store(&peer, &record(root.path(), &nonce, "workers-ready"));
        symlink(&peer, &path).unwrap();
        assert!(observe(registry.path(), &nonce, |_, _| Ok(())).is_err());
        fs::remove_file(&path).unwrap();
        store(&path, &record(root.path(), &nonce, "workers-ready"));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(observe(registry.path(), &nonce, |_, _| Ok(())).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&path, root.path().join("alias")).unwrap();
        assert!(observe(registry.path(), &nonce, |_, _| Ok(())).is_err());
        fs::remove_file(root.path().join("alias")).unwrap();
        fs::write(&path, vec![b' '; 4097]).unwrap();
        assert!(observe(registry.path(), &nonce, |_, _| Ok(())).is_err());
    }
}
