//! A stock user service manager and D-Bus isolated from the host user's services.
use super::*;
use std::{
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
};

pub(super) struct Manager {
    process: std::process::Child,
    environment: Vec<(String, String)>,
}
impl Manager {
    pub(super) fn start(root: &Path, platform: Platform) -> Self {
        assert!(!root.join(".wrix-native-test-resources-stopped").exists());
        let directory = root.join("private-user-manager");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let units = directory.join("units");
        fs::create_dir(&units).unwrap();
        let config = root.join("home/.config");
        fs::create_dir_all(config.join("systemd/user")).unwrap();
        let diagnostic = config.join("systemd/user/wrix-host-controller.service.d");
        fs::create_dir(&diagnostic).unwrap();
        fs::write(
            diagnostic.join("qualification-log.conf"),
            format!(
                "[Service]\nStandardOutput=append:{}\nStandardError=append:{}\n",
                directory.join("controller.log").display(),
                directory.join("controller.log").display()
            ),
        )
        .unwrap();
        fs::write(units.join("basic.target"), "[Unit]\nDescription=Owned public native qualification\nDefaultDependencies=no\nWants=dbus.socket\nAfter=dbus.socket\n").unwrap();
        fs::write(units.join("dbus.socket"), "[Unit]\nDescription=Owned private D-Bus socket\nDefaultDependencies=no\n[Socket]\nListenStream=%t/bus\nSocketMode=0600\n").unwrap();
        fs::write(units.join("dbus.service"), format!("[Unit]\nDescription=Owned stock D-Bus daemon\nRequires=dbus.socket\nAfter=dbus.socket\n[Service]\nType=notify\nExecStart={} --session --address=systemd: --nofork --nopidfile --systemd-activation\n", PathBuf::from(std::env::var_os("WRIX_TEST_POOL_DBUS").unwrap()).display())).unwrap();
        let runtime = root.join("runtime");
        let address = format!("unix:path={}", runtime.join("bus").display());
        let mut environment: Vec<(String, String)> = std::env::vars().collect();
        for (name, value) in [
            (
                "WRIX_MICROVM",
                if platform == Platform::Krun { "1" } else { "0" }.to_owned(),
            ),
            ("DBUS_SESSION_BUS_ADDRESS", address.clone()),
            ("XDG_CONFIG_HOME", config.display().to_string()),
            (
                "SYSTEMD_UNIT_PATH",
                format!(
                    "{}:{}",
                    config.join("systemd/user").display(),
                    units.display()
                ),
            ),
        ] {
            environment.retain(|(key, _)| key != name);
            environment.push((name.to_owned(), value));
        }
        let membership = fs::read_to_string("/proc/self/cgroup").unwrap();
        let current =
            Path::new("/sys/fs/cgroup").join(membership.trim().strip_prefix("0::/").unwrap());
        let delegated=current.ancestors().find(|path| {
            path.file_name().is_some_and(|name| name.to_string_lossy().ends_with(".scope"))
                && fs::metadata(path).is_ok_and(|m| m.uid()==rustix::process::geteuid().as_raw())
                && fs::read_to_string(path.join("cgroup.subtree_control")).is_ok_and(|s| ["cpu","memory","pids"].iter().all(|name|s.split_whitespace().any(|v|v==*name)))
        }).expect("UNAVAILABLE: public qualification needs an already delegated owned task scope; no global service changes");
        let cgroup = delegated.join(format!(
            "wx-doaia-public-{}.scope",
            root.file_name().unwrap().to_str().unwrap()
        ));
        fs::create_dir(&cgroup).unwrap();
        let mut command = Command::new(std::env::var_os("WRIX_TEST_POOL_MANAGER").unwrap());
        command
            .env_clear()
            .envs(environment.iter().cloned())
            .arg(&cgroup)
            .stdout(fs::File::create(directory.join("manager.log")).unwrap());
        let log = fs::OpenOptions::new()
            .append(true)
            .open(directory.join("manager.log"))
            .unwrap();
        command.stderr(log);
        let manager = command.spawn().unwrap();
        let metadata = fs::metadata(&cgroup).unwrap();
        let proof = serde_json::json!({"version":1,"pid":manager.id(),"cgroup":cgroup,"device":metadata.dev(),"inode":metadata.ino(),"executable":PathBuf::from(std::env::var_os("WRIX_TEST_POOL_SYSTEMD").unwrap()).display().to_string(),"start_ticks":fs::read_to_string(format!("/proc/{}/stat",manager.id())).unwrap().rsplit_once(')').unwrap().1.split_whitespace().nth(19).unwrap()});
        let mut receipt = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join("ownership.json"))
            .unwrap();
        receipt
            .write_all(&serde_json::to_vec(&proof).unwrap())
            .unwrap();
        receipt.sync_all().unwrap();
        let mut result = Self {
            process: manager,
            environment,
        };
        let deadline = Instant::now() + BUDGET;
        loop {
            let mut query = Command::new(std::env::var_os("WRIX_TEST_POOL_BUSCTL").unwrap());
            result.configure(&mut query);
            let version = query
                .args([
                    "--address",
                    &address,
                    "--auto-start=no",
                    "--timeout=1",
                    "get-property",
                    "org.freedesktop.systemd1",
                    "/org/freedesktop/systemd1",
                    "org.freedesktop.systemd1.Manager",
                    "Version",
                ])
                .output()
                .unwrap();
            if result
                .systemctl(&["show", "--property=Version"])
                .status
                .success()
                && version.status.success()
                && !version.stdout.is_empty()
            {
                let mut query = Command::new(std::env::var_os("WRIX_TEST_POOL_BUSCTL").unwrap());
                result.configure(&mut query);
                let group = query
                    .args([
                        "--address",
                        &address,
                        "--auto-start=no",
                        "--timeout=1",
                        "get-property",
                        "org.freedesktop.systemd1",
                        "/org/freedesktop/systemd1",
                        "org.freedesktop.systemd1.Manager",
                        "ControlGroup",
                    ])
                    .output()
                    .unwrap();
                assert!(group.status.success());
                let text = String::from_utf8(group.stdout).unwrap();
                let actual: String =
                    serde_json::from_str(text.trim().strip_prefix("s ").unwrap()).unwrap();
                assert_eq!(
                    Path::new("/sys/fs/cgroup").join(actual.trim_start_matches('/')),
                    cgroup
                );
                break;
            }
            assert!(
                result.process.try_wait().unwrap().is_none(),
                "private native manager exited"
            );
            assert!(
                Instant::now() < deadline,
                "private native manager readiness"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        result
    }
    pub(super) fn configure(&self, command: &mut Command) {
        command.env_clear().envs(self.environment.iter().cloned());
    }
    pub(super) fn systemctl(&self, args: &[&str]) -> std::process::Output {
        let mut command = Command::new(std::env::var_os("WRIX_TEST_POOL_SYSTEMCTL").unwrap());
        self.configure(&mut command);
        command.arg("--user").args(args).output().unwrap()
    }
    pub(super) fn controller_pid(&self) -> rustix::process::Pid {
        let output = self.systemctl(&[
            "show",
            "wrix-host-controller.service",
            "--property=MainPID",
            "--value",
        ]);
        assert!(output.status.success());
        let pid: i32 = String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(pid > 1);
        rustix::process::Pid::from_raw(pid).unwrap()
    }
}
// The registered parent fixture owns teardown, including panic/timeout paths.
// Dropping Child deliberately leaves this real manager supervised until that
// parent has stopped its native containers and filesystem readers.

/// Kernel-pinned ownership and unconditional resume on test assertion failure.
pub(super) struct Paused(std::os::fd::OwnedFd);
impl Paused {
    pub(super) fn new(manager: &Manager) -> Self {
        let pid = manager.controller_pid();
        let fd = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty()).unwrap();
        assert_eq!(manager.controller_pid(), pid);
        rustix::process::pidfd_send_signal(&fd, rustix::process::Signal::STOP).unwrap();
        Self(fd)
    }
}
impl Drop for Paused {
    fn drop(&mut self) {
        if let Err(error) =
            rustix::process::pidfd_send_signal(&self.0, rustix::process::Signal::CONT)
        {
            tracing::error!(%error, "owned qualification controller could not resume");
        }
    }
}
