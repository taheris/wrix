//! Select the admitted project's native database, never a sibling/server database.
use super::Error;
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

pub(super) fn locate(workspace: &Path) -> Result<PathBuf, Error> {
    let root = workspace.join(".beads/dolt");
    super::check_directory(&root)?;
    let metadata: serde_json::Value = match fs::read(workspace.join(".beads/metadata.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::Value::Null,
        Err(error) => return Err(error.into()),
    };
    let configured = match metadata.get("dolt_database") {
        Some(value) => {
            let name = value.as_str().ok_or(Error::UnsafePath)?;
            let mut parts = Path::new(name).components();
            if name.len() > 128
                || !matches!(parts.next(), Some(Component::Normal(part)) if part == std::ffi::OsStr::new(name))
                || parts.next().is_some()
            {
                return Err(Error::UnsafePath);
            }
            Some(root.join(name))
        }
        None => None,
    };
    if let Some(path) = &configured {
        super::check_directory(path)?;
        if path.join(".dolt/noms").is_dir() {
            return Ok(path.clone());
        }
    }
    let mut children = Vec::new();
    for entry in fs::read_dir(&root)? {
        let path = entry?.path();
        if fs::symlink_metadata(&path)?.file_type().is_symlink() {
            return Err(Error::UnsafePath);
        }
        if path.join(".dolt/noms").is_dir() {
            children.push(path);
        }
    }
    if root.join(".dolt/noms").is_dir() {
        return if children.is_empty() {
            Ok(root)
        } else {
            Err(Error::SourceAmbiguous { root })
        };
    }
    if configured.is_none() && children.len() == 1 {
        return children.pop().ok_or(Error::SourceMissing { root });
    }
    if children.is_empty() {
        return Err(Error::SourceMissing { root });
    }
    Err(Error::SourceAmbiguous { root })
}

#[cfg(test)]
mod test;
