//! Additional fixture operations used by admission tests.

#![cfg(test)]

use super::fixture::{Fixture, isolation::install_executable};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

impl Fixture {
    pub fn install(&self, name: &str) -> PathBuf {
        let binary = executable(name);
        if self.isolated {
            let destination = Path::new("/bin").join(name);
            install_executable(self.root.path(), &binary, &destination);
            destination
        } else {
            binary
        }
    }

    pub fn git(&self, caller: &Path, args: &[&str]) -> Output {
        let binary = self.install("git");
        let mut command = if self.isolated {
            let mut command = Command::new("chroot");
            command.arg(self.root.path()).arg(binary);
            command
        } else {
            Command::new(binary)
        };
        command
            .arg("-C")
            .arg(caller)
            .args(args)
            .env_clear()
            .env("PATH", "/bin:/usr/bin")
            .env("HOME", self.inside("home"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap()
    }

    pub fn application(&self, caller: &Path, args: &[&str]) -> Output {
        let mut command = if self.isolated {
            install_executable(
                self.root.path(),
                &executable("bash"),
                Path::new("/bin/bash"),
            );
            let wrapper = self.root.path().join("bin/host-run");
            fs::write(
                &wrapper,
                "#!/bin/bash\nset -euo pipefail\ncd \"$1\"\nshift\nexec /bin/wrix \"$@\"\n",
            )
            .unwrap();
            fs::set_permissions(wrapper, fs::Permissions::from_mode(0o700)).unwrap();
            let mut command = Command::new("chroot");
            command
                .arg(self.root.path())
                .arg("/bin/host-run")
                .arg(caller);
            command
        } else {
            let mut command = Command::new(env!("CARGO_BIN_EXE_wrix"));
            command.current_dir(caller);
            command
        };
        command
            .args(args)
            .env_clear()
            .env("HOME", self.inside("home"))
            .env(
                "PATH",
                if self.isolated {
                    std::ffi::OsString::from("/bin")
                } else {
                    std::env::var_os("PATH").unwrap()
                },
            )
            .env("WRIX_CACHE_ENABLE", "0")
            .env("WRIX_DOLT_TRANSPORT", "unix")
            .output()
            .unwrap()
    }
    pub fn inside(&self, relative: &str) -> PathBuf {
        if self.isolated {
            Path::new("/").join(relative)
        } else {
            self.root.path().join(relative)
        }
    }

    pub fn repository(&self, relative: &str) -> PathBuf {
        let directory = self.root.path().join(relative);
        let output = Command::new("git")
            .args(["init", "-q"])
            .arg(&directory)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if self.isolated {
            install_executable(self.root.path(), &executable("git"), Path::new("/bin/git"));
        }
        self.inside(relative)
    }

    pub fn adapter(&self, checkout: &Path, primary: &Path, membership: &str) {
        let response = serde_json::json!({"version":1, "membership":membership,
            "checkout_root_b64":wrix_state::resolver::encode(checkout),
            "primary_root_b64":wrix_state::resolver::encode(primary)});
        self.adapter_script(&format!(
            "IFS= read -r request || [[ -n \"$request\" ]]\nprintf '%s' '{}'\n",
            response.to_string().replace('\'', "'\\''")
        ));
    }

    pub fn adapter_script(&self, body: &str) {
        let bash = executable("bash");
        let selected = if self.isolated {
            install_executable(self.root.path(), &bash, Path::new("/bin/bash"));
            PathBuf::from("/bin/bash")
        } else {
            bash
        };
        let path = self.root.path().join("adapter");
        fs::write(
            &path,
            format!("#!{}\nset -euo pipefail\n{body}", selected.display()),
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        let path = self.inside("adapter");
        let output = self.run(&[
            "integration",
            "select",
            "--slot",
            "test-manager",
            "--executable",
            path.to_str().unwrap(),
        ]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    pub fn admit(&self, directory: &Path, membership: &str, extra: &[&str]) -> Output {
        let mut args = vec![
            "admit",
            "--integration",
            "test-manager",
            "--expect-membership",
            membership,
            "--checkout",
            directory.to_str().unwrap(),
        ];
        args.extend_from_slice(extra);
        let output = self.run(&args);
        if !output.status.success() {
            eprintln!(
                "admission failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        output
    }
}

fn executable(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
        .unwrap()
        .canonicalize()
        .unwrap()
}
