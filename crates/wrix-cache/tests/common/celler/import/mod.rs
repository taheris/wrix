use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    process::{Command, Output},
};

pub struct Store {
    root: tempfile::TempDir,
}

impl Store {
    pub fn new() -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
        }
    }

    pub fn copy(&self, endpoint: &str, token: &str, key: &str, path: &Path) -> Output {
        let root = self.root.path();
        let netrc = root.join("netrc");
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&netrc)
            .unwrap();
        writeln!(file, "machine 127.0.0.1 login fixture password {token}").unwrap();
        Command::new("nix")
            .args([
                "--extra-experimental-features",
                "nix-command",
                "copy",
                "--store",
            ])
            .arg(format!("local?root={}", root.display()))
            .args(["--from", endpoint, "--option", "netrc-file"])
            .arg(netrc)
            .args([
                "--option",
                "require-sigs",
                "true",
                "--option",
                "trusted-public-keys",
                key,
                "--option",
                "substituters",
                "",
                "--option",
                "builders",
                "",
            ])
            .arg(path)
            .env("HOME", root)
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("NIX_CONF_DIR", root.join("no-system-config"))
            .env("NIX_USER_CONF_FILES", "/dev/null")
            .env("NIX_CONFIG", "")
            .env("NIX_REMOTE", "unix:///no-test-store-daemon")
            .output()
            .unwrap()
    }

    pub fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        fs::read(
            self.root
                .path()
                .join("nix/store")
                .join(path.file_name().unwrap()),
        )
    }
}
