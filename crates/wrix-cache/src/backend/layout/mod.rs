//! Host-user Celler data, separate from workspace publication authority.

use std::{
    env,
    path::{Component, Path, PathBuf},
};

use displaydoc::Display;
use thiserror::Error as ThisError;

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// HOME must name an absolute directory for managed Celler data
    Home,
    /// {name} must name an absolute directory without dot components for managed Celler data
    RelativeBase { name: &'static str },
    /// Celler durable metadata and archive roots must be disjoint
    Overlap,
    /// {0}
    Directory(#[from] wrix_core::private_directory::Error),
}

/// Platform layout is explicit so host planning never inherits a guest's conventions.
#[derive(Clone, Copy, Debug)]
pub enum Platform {
    Linux,
    Darwin,
}

/// These directories contain delegated signing/archive state, never controller credentials.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Layout {
    metadata: PathBuf,
    archives: PathBuf,
}

/// One writable service data mapping, not an arbitrary host directory grant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Mount<'a> {
    pub source: &'a Path,
    pub destination: &'static str,
}

impl Layout {
    /// Resolves paths without creating state or reading workspace policy.
    pub fn current() -> Result<Self, Error> {
        let home = env::var_os("HOME").map(PathBuf::from).ok_or(Error::Home)?;
        let platform = if cfg!(target_os = "macos") {
            Platform::Darwin
        } else {
            Platform::Linux
        };
        Self::resolve(
            platform,
            &home,
            env::var_os("XDG_STATE_HOME").as_deref().map(Path::new),
            env::var_os("XDG_CACHE_HOME").as_deref().map(Path::new),
        )
    }

    /// XDG overrides apply only on Linux; neither workspace identity nor generation changes these paths.
    pub fn resolve(
        platform: Platform,
        home: &Path,
        state_override: Option<&Path>,
        cache_override: Option<&Path>,
    ) -> Result<Self, Error> {
        if !absolute_base(home) {
            return Err(Error::Home);
        }
        let (state, cache) = match platform {
            Platform::Linux => (
                state_override.map_or_else(|| home.join(".local/state"), Path::to_path_buf),
                cache_override.map_or_else(|| home.join(".cache"), Path::to_path_buf),
            ),
            Platform::Darwin => (
                home.join("Library/Application Support"),
                home.join("Library/Caches"),
            ),
        };
        if !absolute_base(&state) {
            return Err(Error::RelativeBase {
                name: "XDG_STATE_HOME",
            });
        }
        if !absolute_base(&cache) {
            return Err(Error::RelativeBase {
                name: "XDG_CACHE_HOME",
            });
        }
        let layout = Self {
            metadata: state.join("wrix/service-data/celler"),
            archives: cache.join("wrix/celler/archives"),
        };
        if layout.metadata.starts_with(&layout.archives)
            || layout.archives.starts_with(&layout.metadata)
        {
            return Err(Error::Overlap);
        }
        Ok(layout)
    }

    pub fn metadata(&self) -> &Path {
        &self.metadata
    }
    pub fn archives(&self) -> &Path {
        &self.archives
    }

    /// Creates only delegated Celler data; it neither generates keys nor starts or collects a backend.
    pub fn prepare(&self) -> Result<(), Error> {
        wrix_core::private_directory::prepare_with_parents(&self.metadata)?;
        wrix_core::private_directory::prepare_with_parents(&self.archives)?;
        Ok(())
    }

    /// Stable narrow mappings exclude workspace control state, host store and issuer/publisher files.
    pub fn mounts(&self) -> [Mount<'_>; 2] {
        [
            Mount {
                source: &self.metadata,
                destination: "/var/lib/wrix/celler",
            },
            Mount {
                source: &self.archives,
                destination: "/var/cache/wrix/celler",
            },
        ]
    }
}

fn absolute_base(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
}

#[cfg(test)]
mod test {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    #[test]
    fn linux_default_and_explicit_bases_are_host_user_scoped() {
        let home = Path::new("/home/operator");
        let defaults = Layout::resolve(Platform::Linux, home, None, None).unwrap();
        assert_eq!(
            defaults.metadata(),
            home.join(".local/state/wrix/service-data/celler")
        );
        assert_eq!(
            defaults.archives(),
            home.join(".cache/wrix/celler/archives")
        );
        let selected = Layout::resolve(
            Platform::Linux,
            home,
            Some(Path::new("/state")),
            Some(Path::new("/cache")),
        )
        .unwrap();
        assert_eq!(
            selected.metadata(),
            Path::new("/state/wrix/service-data/celler")
        );
        assert_eq!(
            selected.archives(),
            Path::new("/cache/wrix/celler/archives")
        );
    }

    #[test]
    fn darwin_ignores_xdg_overrides() {
        let home = Path::new("/Users/operator");
        let layout = Layout::resolve(
            Platform::Darwin,
            home,
            Some(Path::new("relative")),
            Some(Path::new("relative")),
        )
        .unwrap();
        assert_eq!(
            layout.metadata(),
            home.join("Library/Application Support/wrix/service-data/celler")
        );
        assert_eq!(
            layout.archives(),
            home.join("Library/Caches/wrix/celler/archives")
        );
    }

    #[test]
    fn resolution_does_not_create_state() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("absent-home");
        let layout = Layout::resolve(Platform::Linux, &home, None, None).unwrap();
        assert!(!home.exists());
        assert!(!layout.metadata().exists());
        assert!(!layout.archives().exists());
    }

    #[test]
    fn preparation_preserves_backend_state_and_has_only_narrow_data_mounts() {
        let root = tempfile::tempdir().unwrap();
        let layout = Layout::resolve(Platform::Linux, root.path(), None, None).unwrap();
        layout.prepare().unwrap();
        fs::write(
            layout.metadata().join("metadata.db"),
            "database and valid keys",
        )
        .unwrap();
        fs::write(layout.archives().join("retained"), "archive").unwrap();
        layout.prepare().unwrap();
        assert_eq!(
            fs::read_to_string(layout.metadata().join("metadata.db")).unwrap(),
            "database and valid keys"
        );
        assert_eq!(
            fs::read_to_string(layout.archives().join("retained")).unwrap(),
            "archive"
        );
        let mounts = layout.mounts();
        assert_eq!(mounts[0].source, layout.metadata());
        assert_eq!(mounts[0].destination, "/var/lib/wrix/celler");
        assert_eq!(mounts[1].source, layout.archives());
        assert_eq!(mounts[1].destination, "/var/cache/wrix/celler");
        for mount in mounts {
            assert_eq!(
                fs::metadata(mount.source).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert!(
                !mount
                    .source
                    .starts_with(root.path().join(".local/state/wrix/workspaces"))
            );
            assert!(!mount.source.join("celler-publisher.json").exists());
            assert!(!mount.source.join("publish-roots.json").exists());
        }
    }

    #[test]
    fn preparation_rejects_symlinked_backend_directories() {
        let root = tempfile::tempdir().unwrap();
        let layout = Layout::resolve(Platform::Linux, root.path(), None, None).unwrap();
        let target = root.path().join("unrelated");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir_all(layout.metadata().parent().unwrap()).unwrap();
        symlink(&target, layout.metadata()).unwrap();
        assert!(layout.prepare().is_err());
        assert!(!layout.archives().exists());
        assert_eq!(
            fs::metadata(target).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn overlapping_data_roots_are_rejected_before_state_creation() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("wrix/service-data/celler");
        assert!(matches!(
            Layout::resolve(
                Platform::Linux,
                root.path(),
                Some(root.path()),
                Some(&nested)
            ),
            Err(Error::Overlap)
        ));
        assert!(!nested.exists());
    }

    #[test]
    fn parent_traversal_in_bases_is_rejected_before_state_creation() {
        let root = tempfile::tempdir().unwrap();
        let escaping = root.path().join("absent/../other");
        assert!(Layout::resolve(Platform::Linux, root.path(), Some(&escaping), None).is_err());
        assert!(Layout::resolve(Platform::Linux, root.path(), None, Some(&escaping)).is_err());
        assert!(!root.path().join("absent").exists());
        assert!(!root.path().join("other").exists());
    }

    #[test]
    fn relative_overrides_are_rejected_before_state_creation() {
        assert!(Layout::resolve(Platform::Linux, Path::new("relative"), None, None).is_err());
        for (state, cache) in [
            (Some(Path::new("relative")), None),
            (None, Some(Path::new("relative"))),
        ] {
            assert!(
                Layout::resolve(Platform::Linux, Path::new("/home/operator"), state, cache)
                    .is_err()
            );
        }
    }
}
