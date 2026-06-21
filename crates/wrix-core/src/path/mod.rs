use std::{
    ffi::OsStr,
    fmt, io,
    path::{Component, Path, PathBuf},
};

use sha2::{Digest, Sha256};

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

    pub fn from_service_current_dir() -> io::Result<Self> {
        let current_dir = std::env::current_dir()?;
        Self::from_service_path(current_dir)
    }

    pub fn from_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let canonical_path = path.as_ref().canonicalize()?;
        Ok(Self::from_canonical_path(canonical_path))
    }

    pub fn from_service_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let canonical_path = path.as_ref().canonicalize()?;
        Ok(Self::from_canonical_path(service_workspace_path(
            &canonical_path,
        )))
    }

    fn from_canonical_path(canonical_path: PathBuf) -> Self {
        let hash = WorkspaceHash::from_path(&canonical_path);
        Self {
            canonical_path,
            hash,
        }
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
    pub const HEX_LEN: usize = 64;

    fn from_path(path: &Path) -> Self {
        let identity = path.as_os_str().to_string_lossy();
        let digest = Sha256::digest(identity.as_bytes());
        let mut hash = String::with_capacity(Self::HEX_LEN);
        for byte in digest {
            push_hex_byte(&mut hash, byte);
        }
        Self(hash)
    }

    pub const fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn is_valid_str(value: &str) -> bool {
        value.len() == Self::HEX_LEN && value.bytes().all(is_lower_hex_digit)
    }

    pub fn port_offset(&self, width: u16) -> u16 {
        if width == 0 {
            return 0;
        }
        let mut value = 0_u16;
        for byte in self.0.bytes().take(16) {
            let next = (u32::from(value) * 16 + u32::from(hex_nibble(byte))) % u32::from(width);
            value = u16::try_from(next).map_or(0, |offset| offset);
        }
        value
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

fn push_hex_byte(output: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    output.push(char::from(HEX[usize::from(byte >> 4)]));
    output.push(char::from(HEX[usize::from(byte & 0x0f)]));
}

const fn is_lower_hex_digit(byte: u8) -> bool {
    matches!(byte, b'0'..=b'9' | b'a'..=b'f')
}

const fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => 0,
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

fn service_workspace_path(path: &Path) -> PathBuf {
    loom_control_root(path)
        .or_else(|| repository_root(path))
        .unwrap_or_else(|| path.to_path_buf())
}

fn loom_control_root(path: &Path) -> Option<PathBuf> {
    let components = path.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        if component.as_os_str() != OsStr::new(".loom") {
            continue;
        }
        let prefix = path_from_components(&components[..index]);
        if prefix.as_os_str().is_empty() {
            return None;
        }
        if let Some(root) = repository_root(&prefix) {
            return Some(root);
        }
    }
    None
}

fn repository_root(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|ancestor| has_repository_marker(ancestor))
        .map(Path::to_path_buf)
}

fn has_repository_marker(path: &Path) -> bool {
    let marker = path.join(".git");
    marker.is_dir() || marker.is_file()
}

fn path_from_components(components: &[Component<'_>]) -> PathBuf {
    let mut path = PathBuf::new();
    for component in components {
        path.push(component.as_os_str());
    }
    path
}

#[cfg(test)]
mod test {
    use std::{fs, path::Path};

    use super::{Workspace, WorkspaceHash, sanitize_container_component};

    #[test]
    fn workspace_hash_is_sha256_of_identity_path() {
        let hash = WorkspaceHash::from_path(Path::new("/workspace"));
        assert_eq!(
            hash.as_str(),
            "c52ddf65534b7b46035084358ab7902be4bfef220bdb503ac7039cc861905b05"
        );
        assert!(WorkspaceHash::is_valid_str(hash.as_str()));
        assert_eq!(hash.port_offset(2_000), 1_542);
    }

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
        assert!(WorkspaceHash::is_valid_str(direct.hash().as_str()));
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
    fn service_workspace_uses_repository_root_for_subdirectory() {
        let root = tempfile_root("service-repo-root");
        let child = root.join("src/bin");
        fs::create_dir(root.join(".git")).unwrap();
        fs::create_dir_all(&child).unwrap();
        let workspace = Workspace::from_service_path(child).unwrap();
        assert_eq!(workspace.canonical_path(), root.canonicalize().unwrap());
        assert_eq!(
            workspace.container_name().as_str(),
            "service-repo-root-service"
        );
    }

    #[test]
    fn service_workspace_uses_outer_repository_for_loom_clone() {
        let root = tempfile_root("loom-service-repo");
        let bead = root.join(".loom/beads/lm-gzgw.3");
        fs::create_dir(root.join(".git")).unwrap();
        fs::create_dir_all(bead.join(".git")).unwrap();
        let workspace = Workspace::from_service_path(&bead).unwrap();
        let clone_workspace = Workspace::from_path(bead).unwrap();
        assert_eq!(workspace.canonical_path(), root.canonicalize().unwrap());
        assert_eq!(
            workspace.container_name().as_str(),
            "loom-service-repo-service"
        );
        assert_eq!(
            workspace.hash(),
            Workspace::from_path(&root).unwrap().hash()
        );
        assert_ne!(workspace.hash(), clone_workspace.hash());
        assert_eq!(
            clone_workspace.container_name().as_str(),
            "lm-gzgw.3-service"
        );
    }

    #[test]
    fn service_workspace_uses_outer_repository_for_loom_integration() {
        let root = tempfile_root("loom-integration-repo");
        let integration = root.join(".loom/integration");
        fs::create_dir(root.join(".git")).unwrap();
        fs::create_dir_all(&integration).unwrap();
        let workspace = Workspace::from_service_path(&integration).unwrap();
        let integration_workspace = Workspace::from_path(integration).unwrap();
        assert_eq!(workspace.canonical_path(), root.canonicalize().unwrap());
        assert_eq!(
            workspace.hash(),
            Workspace::from_path(&root).unwrap().hash()
        );
        assert_ne!(workspace.hash(), integration_workspace.hash());
        assert_eq!(
            workspace.container_name().as_str(),
            "loom-integration-repo-service"
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
