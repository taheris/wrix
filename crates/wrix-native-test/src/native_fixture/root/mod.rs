use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::{
        ffi::OsStringExt,
        fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

pub const MARKER: &str = ".wrix-native-test-fixture";
pub const STOPPED: &str = ".wrix-native-test-resources-stopped";

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// native fixture filesystem failed: {0}
    Io(#[from] io::Error),
    /// native fixture receipt failed: {0}
    Json(#[from] serde_json::Error),
    /// unsafe fixture root: {0}
    Root(PathBuf),
    /// fixture path escapes its private root: {0}
    Escape(PathBuf),
    /// fixture registry must be an owned private directory
    Registry,
    /// invalid fixture registry entry: {0}
    Entry(PathBuf),
    /// unsafe native test teardown receipt
    Receipt,
    /// native test teardown receipt belongs to another root
    Foreign,
    /// fixture identity changed after admission
    Replaced,
}

#[derive(Clone, Copy, Debug)]
pub enum Origin {
    Registered,
    Discovered,
}

#[derive(Debug)]
pub struct Root {
    path: PathBuf,
    identity: Identity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    version: u8,
    device: u64,
    inode: u64,
}
impl Identity {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            version: 1,
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

pub fn legacy(path: &Path) -> bool {
    path.parent() == Some(Path::new("/tmp"))
        && path.file_name().is_some_and(|name| {
            let bytes = name.as_encoded_bytes();
            bytes
                .strip_prefix(b".tmp")
                .is_some_and(|tail| !tail.is_empty() && tail.iter().all(u8::is_ascii_alphanumeric))
        })
}

pub fn private_directory(path: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || fs::canonicalize(path)? != path
    {
        return Err(Error::Registry);
    }
    Ok(())
}

pub fn open_private(path: &Path, limit: u64) -> Result<File, Error> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 1
        || metadata.len() > limit
    {
        return Err(Error::Receipt);
    }
    Ok(file)
}

pub fn registry(path: &Path) -> Result<BTreeMap<PathBuf, PathBuf>, Error> {
    private_directory(path)?;
    let mut entries = BTreeMap::new();
    for entry in fs::read_dir(path)? {
        let path = entry?.path();
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
            .open(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != rustix::process::getuid().as_raw()
            || metadata.len() > 4096
        {
            return Err(Error::Entry(path));
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file).take(4097).read_to_end(&mut bytes)?;
        if bytes.len() > 4096 {
            return Err(Error::Entry(path));
        }
        entries.insert(PathBuf::from(OsString::from_vec(bytes)), path);
    }
    Ok(entries)
}

impl Root {
    pub fn parse(path: &Path, origin: Origin) -> Result<Self, Error> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir()
            || metadata.uid() != rustix::process::getuid().as_raw()
            || metadata.mode() & 0o022 != 0
            || fs::canonicalize(path)? != path
        {
            return Err(Error::Root(path.to_owned()));
        }
        let marked = path.join(MARKER).is_file() || matches!(origin, Origin::Registered);
        if !marked {
            let control = path.join("home/.local/state/wrix/control/nix");
            if !legacy(path) || !control.is_dir() || control.is_symlink() {
                return Err(Error::Root(path.to_owned()));
            }
        }
        let root = Self {
            path: path.to_owned(),
            identity: Identity::of(&metadata),
        };
        for child in ["home", "runtime", "home/.local/share/containers/storage"] {
            root.child(child)?;
        }
        Ok(root)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn recheck(&self) -> Result<(), Error> {
        let metadata = fs::symlink_metadata(&self.path)?;
        if !metadata.is_dir()
            || Identity::of(&metadata) != self.identity
            || metadata.uid() != rustix::process::getuid().as_raw()
            || metadata.mode() & 0o022 != 0
        {
            return Err(Error::Replaced);
        }
        Ok(())
    }
    pub fn child(&self, relative: impl AsRef<Path>) -> Result<PathBuf, Error> {
        self.recheck()?;
        let relative = relative.as_ref();
        if relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(Error::Escape(relative.to_owned()));
        }
        let mut candidate = self.path.clone();
        for component in relative.components() {
            candidate.push(component);
            match fs::symlink_metadata(&candidate) {
                Ok(_) => {
                    let canonical = fs::canonicalize(&candidate)?;
                    if !canonical.starts_with(&self.path) {
                        return Err(Error::Escape(candidate));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(candidate)
    }
    pub fn stopped(&self) -> Result<bool, Error> {
        self.recheck()?;
        let path = self.path.join(STOPPED);
        let file = match open_private(&path, 4096) {
            Ok(file) => file,
            Err(Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let identity: Identity = serde_json::from_reader(file)?;
        if identity != self.identity {
            return Err(Error::Foreign);
        }
        Ok(true)
    }
    pub fn record_stopped(&self) -> Result<(), Error> {
        self.recheck()?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".cleanup-")
            .tempfile_in(&self.path)?;
        serde_json::to_writer(temporary.as_file_mut(), &self.identity)?;
        temporary.as_file_mut().flush()?;
        temporary.as_file().sync_all()?;
        self.recheck()?;
        temporary
            .persist(self.path.join(STOPPED))
            .map_err(|error| error.error)?;
        File::open(&self.path)?.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(MARKER), "1\n").unwrap();
        root
    }
    #[test]
    fn symlinked_or_writable_roots_are_not_cleanup_authority() {
        let parent = tempfile::tempdir().unwrap();
        let root = fixture();
        symlink(root.path(), parent.path().join("alias")).unwrap();
        assert!(Root::parse(&parent.path().join("alias"), Origin::Registered).is_err());
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o777)).unwrap();
        assert!(Root::parse(root.path(), Origin::Registered).is_err());
    }
    #[test]
    fn marked_private_roots_need_no_legacy_name_but_child_aliases_still_fail() {
        let root = fixture();
        let peer = fixture();
        Root::parse(root.path(), Origin::Discovered).unwrap();
        symlink(peer.path(), root.path().join("home")).unwrap();
        assert!(Root::parse(root.path(), Origin::Registered).is_err());
        assert!(peer.path().exists());
    }
    #[test]
    fn missing_owned_children_are_safe_but_traversal_and_absolute_paths_are_not() {
        let root = fixture();
        let selected = Root::parse(root.path(), Origin::Registered).unwrap();
        assert_eq!(
            selected.child("runtime/containers").unwrap(),
            root.path().join("runtime/containers")
        );
        for name in ["../peer", "/peer", "home/../../peer"] {
            assert!(selected.child(name).is_err());
        }
    }
    #[test]
    fn stopped_receipt_is_private_identity_bound_and_reusable() {
        let root = fixture();
        let selected = Root::parse(root.path(), Origin::Discovered).unwrap();
        assert!(!selected.stopped().unwrap());
        selected.record_stopped().unwrap();
        assert!(selected.stopped().unwrap());
        assert_eq!(
            fs::metadata(root.path().join(STOPPED)).unwrap().mode() & 0o777,
            0o600
        );
        let mut identity = selected.identity;
        identity.inode += 1;
        fs::write(
            root.path().join(STOPPED),
            serde_json::to_vec(&identity).unwrap(),
        )
        .unwrap();
        assert!(matches!(selected.stopped(), Err(Error::Foreign)));
    }
    #[test]
    fn symlink_hardlink_unknown_fields_and_public_receipts_cannot_prove_teardown() {
        let root = fixture();
        let selected = Root::parse(root.path(), Origin::Registered).unwrap();
        let receipt = root.path().join(STOPPED);
        selected.record_stopped().unwrap();
        fs::set_permissions(&receipt, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(selected.stopped().is_err());
        selected.record_stopped().unwrap();
        fs::hard_link(&receipt, root.path().join("peer")).unwrap();
        assert!(selected.stopped().is_err());
        selected.record_stopped().unwrap();
        fs::write(
            &receipt,
            format!(
                "{{\"version\":1,\"device\":{},\"inode\":{},\"extra\":true}}",
                selected.identity.device, selected.identity.inode
            ),
        )
        .unwrap();
        assert!(selected.stopped().is_err());
        fs::remove_file(&receipt).unwrap();
        symlink(root.path().join("peer"), &receipt).unwrap();
        assert!(selected.stopped().is_err());
    }
    #[test]
    fn root_replacement_invalidates_previously_selected_cleanup() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("fixture");
        fs::create_dir(&path).unwrap();
        let selected = Root::parse(&path, Origin::Registered).unwrap();
        fs::rename(&path, parent.path().join("old")).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(matches!(selected.recheck(), Err(Error::Replaced)));
    }
    #[test]
    fn registry_requires_owned_private_directory_and_bounded_regular_entries() {
        let parent = tempfile::tempdir().unwrap();
        let registry_path = parent.path().join("registry");
        fs::create_dir(&registry_path).unwrap();
        fs::set_permissions(&registry_path, fs::Permissions::from_mode(0o700)).unwrap();
        let entry = registry_path.join("root");
        fs::write(&entry, b"/fixture").unwrap();
        assert_eq!(
            registry(&registry_path).unwrap().get(Path::new("/fixture")),
            Some(&entry)
        );
        fs::write(&entry, vec![b'a'; 4097]).unwrap();
        assert!(registry(&registry_path).is_err());
        fs::remove_file(&entry).unwrap();
        symlink("/foreign", &entry).unwrap();
        assert!(registry(&registry_path).is_err());
        fs::remove_file(entry).unwrap();
        fs::set_permissions(&registry_path, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(registry(&registry_path).is_err());
    }
}
