use std::{
    ffi::OsStr,
    fmt, io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Workspace {
    canonical_path: PathBuf,
    hash: WorkspaceHash,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceHash(String);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContainerName(String);

impl Workspace {
    pub fn from_current_dir() -> io::Result<Self> {
        let current_dir = std::env::current_dir()?;
        Self::from_path(current_dir)
    }

    pub fn from_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let canonical_path = path.as_ref().canonicalize()?;
        let hash = WorkspaceHash::from_path(&canonical_path);
        Ok(Self {
            canonical_path,
            hash,
        })
    }

    pub fn canonical_path(&self) -> &Path {
        self.canonical_path.as_path()
    }

    pub fn repository_name(&self) -> Option<&OsStr> {
        self.canonical_path.file_name()
    }

    pub const fn hash(&self) -> &WorkspaceHash {
        &self.hash
    }

    pub fn container_name(&self) -> ContainerName {
        let repository = self
            .repository_name()
            .and_then(OsStr::to_str)
            .map(sanitize_container_component)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| String::from("workspace"));
        ContainerName(format!("{repository}-service"))
    }
}

impl WorkspaceHash {
    fn from_path(path: &Path) -> Self {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for byte in path.as_os_str().to_string_lossy().as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Self(format!("{hash:016x}"))
    }

    pub const fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn port_offset(&self, width: u16) -> u16 {
        let mut value = 0_u16;
        for byte in self.0.bytes().take(4) {
            value = value.wrapping_mul(16);
            value = value.wrapping_add(match byte {
                b'0'..=b'9' => u16::from(byte - b'0'),
                b'a'..=b'f' => u16::from(byte - b'a' + 10),
                _ => 0,
            });
        }
        value % width
    }
}

impl ContainerName {
    pub const fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for WorkspaceHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl fmt::Display for ContainerName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

fn sanitize_container_component(input: &str) -> String {
    input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches(|ch| matches!(ch, '.' | '-'))
        .to_owned()
}

#[cfg(test)]
mod test {
    use std::{fs, path::Path};

    use super::{Workspace, sanitize_container_component};

    #[test]
    fn same_canonical_workspace_has_same_hash() {
        let root = tempfile_root("same-canonical-workspace-has-same-hash");
        let link = root.with_file_name("same-canonical-workspace-has-same-hash-link");
        if link.exists() {
            fs::remove_dir_all(&link).unwrap();
        }
        std::os::unix::fs::symlink(&root, &link).unwrap();
        let direct = Workspace::from_path(&root).unwrap();
        let via_link = Workspace::from_path(&link).unwrap();
        assert_eq!(direct.canonical_path(), via_link.canonical_path());
        assert_eq!(direct.hash(), via_link.hash());
        fs::remove_dir_all(link).unwrap();
    }

    #[test]
    fn different_workspace_paths_have_different_hashes() {
        let first = tempfile_root("first-workspace-path");
        let second = tempfile_root("second-workspace-path");
        let first_workspace = Workspace::from_path(first).unwrap();
        let second_workspace = Workspace::from_path(second).unwrap();
        assert_ne!(first_workspace.hash(), second_workspace.hash());
    }

    #[test]
    fn container_name_uses_repository_name() {
        let root = tempfile_root("container-repo");
        let workspace = Workspace::from_path(root).unwrap();
        assert_eq!(
            workspace.container_name().as_str(),
            "container-repo-service"
        );
    }

    #[test]
    fn container_component_replaces_unsupported_characters() {
        assert_eq!(sanitize_container_component("repo name"), "repo-name");
        assert_eq!(sanitize_container_component(".repo."), "repo");
    }

    fn tempfile_root(name: &str) -> std::path::PathBuf {
        let parent = std::env::temp_dir().join(format!("wrix-path-test-{}", std::process::id()));
        let path = parent.join(name);
        if path.exists() {
            fs::remove_dir_all(&path).unwrap();
        }
        fs::create_dir_all(&path).unwrap();
        assert!(Path::new(&path).is_dir());
        path
    }
}
