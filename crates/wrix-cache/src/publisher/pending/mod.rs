//! Pending native roots are identified by their exact acquisition directory, never by age.
use super::{Error, Paths, Pending, Result, record_pending_named, remove_pending};
use crate::backend::publication::protection::Protection;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn capture(
    paths: &Paths,
    drv: &str,
    outputs: &[String],
    scope: Option<&str>,
) -> Result<PathBuf> {
    if outputs.is_empty() {
        return Err(Error::PendingProtection);
    }
    let selected = outputs.iter().map(PathBuf::from).collect::<Vec<_>>();
    let protection = Protection::acquire(&paths.state_root, &selected)
        .map_err(crate::backend::publication::Error::from)?;
    let name = protection
        .directory()
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(Error::PendingProtection)?;
    let filename = scope.map_or_else(
        || format!("protected-{name}.json"),
        |scope| format!("held-{scope}-{name}.json"),
    );
    record_pending_named(paths, drv, outputs, &filename)
}

pub fn held(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.as_encoded_bytes().starts_with(b"held-"))
}

pub fn ready(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.as_encoded_bytes().starts_with(b"ready-"))
}

pub fn protected(path: &Path) -> bool {
    protection_name(path).is_some()
}

fn protection_name(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?.strip_suffix(".json")?;
    if !name.starts_with("protected-") && !name.starts_with("held-") && !name.starts_with("ready-")
    {
        return None;
    }
    let index = name.rfind("inflight-")?;
    let directory = &name[index..];
    directory
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        .then_some(directory)
}

pub fn approve(paths: &Paths, scope: &str) -> Result<()> {
    let prefix = format!("held-{scope}-");
    for (path, record) in super::pending_records(paths)?.0 {
        let Some(name) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix(&prefix))
        else {
            continue;
        };
        let approved = paths.pending_dir().join(format!("ready-{scope}-{name}"));
        super::write_json(&approved, &record)?;
        remove_pending(&path)?;
    }
    Ok(())
}

pub fn complete(paths: &Paths, record: &Pending) -> Result<()> {
    if let Some(name) = protection_name(&record.path) {
        let directory = paths.state_root.join("publication-roots").join(name);
        if directory.exists() {
            let metadata = fs::symlink_metadata(&directory)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(Error::PendingProtection);
            }
            for (index, path) in record.out_paths.iter().enumerate() {
                if fs::read_link(directory.join(index.to_string()))? != Path::new(path) {
                    return Err(Error::PendingProtection);
                }
            }
            if fs::read_dir(&directory)?.count() != record.out_paths.len() {
                return Err(Error::PendingProtection);
            }
            fs::remove_dir_all(directory)?;
        }
    }
    remove_pending(&record.path)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn protection_identity_is_exact_and_never_inferred_from_legacy_names() {
        for prefix in ["protected-", "held-scope-", "ready-scope-"] {
            let filename = format!("{prefix}inflight-aB0123.json");
            assert_eq!(
                protection_name(Path::new(&filename)),
                Some("inflight-aB0123")
            );
        }
        for filename in [
            "legacy-inflight-aB0123.json",
            "protected-inflight-a%2f.json",
            "protected-other.json",
        ] {
            assert_eq!(protection_name(Path::new(filename)), None);
        }
    }
}
