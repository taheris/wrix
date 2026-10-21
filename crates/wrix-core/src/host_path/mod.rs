//! Platform-native host roots, independent of workspace membership and filesystem effects.

use std::{
    env, io,
    path::{Path, PathBuf},
};

mod source;
pub use source::MountAccess;

#[derive(Clone, Debug)]
pub struct Roots {
    state: PathBuf,
    cache: PathBuf,
}

impl Roots {
    pub fn discover() -> io::Result<Self> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| io::Error::other("HOME is required to select native Wrix roots"))?;
        Self::from_environment(&home, cfg!(target_os = "macos"), |name| env::var_os(name))
    }

    pub fn from_environment(
        home: &Path,
        darwin: bool,
        variable: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> io::Result<Self> {
        let (state, cache) = if darwin {
            (
                home.join("Library/Application Support/wrix"),
                home.join("Library/Caches/wrix"),
            )
        } else {
            (
                variable("XDG_STATE_HOME")
                    .filter(|value| !value.is_empty())
                    .map_or_else(|| home.join(".local/state"), PathBuf::from)
                    .join("wrix"),
                variable("XDG_CACHE_HOME")
                    .filter(|value| !value.is_empty())
                    .map_or_else(|| home.join(".cache"), PathBuf::from)
                    .join("wrix"),
            )
        };
        if !home.is_absolute() || !state.is_absolute() || !cache.is_absolute() {
            return Err(io::Error::other("native Wrix roots must be absolute"));
        }
        Ok(Self { state, cache })
    }

    pub fn state(&self) -> &Path {
        &self.state
    }
    pub fn cache(&self) -> &Path {
        &self.cache
    }
    /// No worker mount may contain host control state, even through a path alias.
    pub fn protect_mount(&self, source: &Path) -> io::Result<()> {
        let source = source.canonicalize()?;
        let state = anchored(&self.state)?;
        if source.starts_with(&state) || state.starts_with(&source) {
            return Err(io::Error::other(
                "mount would expose private Wrix host state",
            ));
        }
        Ok(())
    }

    pub fn database(&self) -> PathBuf {
        self.state.join("state.db")
    }
    pub fn workspace_state(&self, key: &crate::path::WorkspaceHash) -> PathBuf {
        self.state.join("workspaces").join(key.as_str())
    }
    pub fn workspace_cache(&self, key: &crate::path::WorkspaceHash) -> PathBuf {
        self.cache
            .join("workspaces")
            .join(key.as_str())
            .join("binary-cache")
    }
}

/// Observe existing ancestors without creating a missing native path.
pub fn anchored(path: &Path) -> io::Result<PathBuf> {
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or(error)?;
            let name = path
                .file_name()
                .ok_or_else(|| io::Error::other("invalid native state root"))?;
            Ok(anchored(parent)?.join(name))
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod test {
    use super::Roots;
    use std::path::Path;

    #[test]
    fn native_roots_are_pure_and_platform_specific() {
        let home = Path::new("/home/example");
        let linux = Roots::from_environment(home, false, |_| None).unwrap();
        assert_eq!(linux.database(), home.join(".local/state/wrix/state.db"));
        let darwin = Roots::from_environment(home, true, |_| Some("/ignored".into())).unwrap();
        assert_eq!(
            darwin.state(),
            home.join("Library/Application Support/wrix")
        );
        assert_eq!(darwin.cache(), home.join("Library/Caches/wrix"));
    }

    #[test]
    fn relative_overrides_are_rejected() {
        assert!(
            Roots::from_environment(Path::new("/home/example"), false, |_| Some(
                "relative".into()
            ))
            .is_err()
        );
    }
}
