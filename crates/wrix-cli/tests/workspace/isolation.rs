//! Execute unmodified host binaries across an actual filesystem isolation boundary.
#![cfg(test)]
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

pub fn root(binary: &Path) -> (tempfile::TempDir, bool) {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("home")).unwrap();
    let isolated = Path::new("/etc/wrix/image-agent").exists();
    if isolated {
        install_executable(root.path(), binary, Path::new("/bin/wrix"));
        fs::create_dir_all(root.path().join("dev")).unwrap();
        fs::write(root.path().join("dev/urandom"), []).unwrap();
        fs::write(root.path().join("dev/null"), []).unwrap();
    }
    (root, isolated)
}

pub fn command(root: &Path, isolated: bool, executable: &Path, args: &[&str]) -> Command {
    command_as(root, isolated, executable, args, None)
}

pub fn command_as(
    root: &Path,
    isolated: bool,
    executable: &Path,
    args: &[&str],
    owner: Option<(rustix::process::Uid, rustix::process::Gid)>,
) -> Command {
    use std::os::unix::process::CommandExt;
    let mut command = if isolated {
        let mut command = Command::new(parent_binary("unshare"));
        command
            .args([
                "--user",
                "--map-root-user",
                "--mount",
                "--propagation",
                "private",
                "--",
            ])
            .arg(parent_binary("bash"))
            .arg("-c")
            .arg(include_str!("../fixtures/workspace-chroot.sh"))
            .arg("workspace-chroot")
            .arg(parent_binary("mount"))
            .arg(root)
            .arg(chroot_binary());
        if let Some((uid, gid)) = owner {
            command.arg(format!("--userspec={}:{}", uid.as_raw(), gid.as_raw()));
        }
        command.arg(root).arg(executable);
        command
    } else {
        let mut command = Command::new(executable);
        if let Some((uid, gid)) = owner {
            command.uid(uid.as_raw()).gid(gid.as_raw());
        }
        command
    };
    command
        .args(args)
        .env_clear()
        .env(
            "HOME",
            if isolated {
                PathBuf::from("/home")
            } else {
                root.join("home")
            },
        )
        .env(
            "PATH",
            if isolated {
                std::ffi::OsString::from("/bin")
            } else {
                std::env::var_os("PATH").unwrap()
            },
        );
    command
}

pub fn chroot_binary() -> PathBuf {
    parent_binary("chroot")
}

fn parent_binary(name: &str) -> PathBuf {
    let path = std::env::var_os("PATH").expect("parent fixture PATH is required");
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(name);
        match fs::metadata(&candidate) {
            Ok(metadata) if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 => {
                return if candidate.is_absolute() {
                    candidate
                } else {
                    std::env::current_dir().unwrap().join(candidate)
                };
            }
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => panic!("cannot inspect fixture {name} {candidate:?}: {error}"),
        }
    }
    panic!("{name} is unavailable in the parent fixture PATH");
}

#[test]
#[cfg(target_os = "linux")]
fn parent_chroot_lookup_preserves_multicall_applet_identity() {
    let output = Command::new(chroot_binary())
        .arg("--version")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("chroot "));
}

#[test]
#[cfg(target_os = "linux")]
fn isolated_devices_provide_fresh_entropy_and_discard_null_writes() {
    use std::{io::Read, os::unix::fs::FileTypeExt};

    const CHILD: &str = "WRIX_TEST_ISOLATED_DEVICES";
    if std::env::var_os(CHILD).is_some() {
        assert!(!Path::new("/etc/wrix/image-agent").exists());
        for device in ["/dev/urandom", "/dev/null"] {
            assert!(fs::metadata(device).unwrap().file_type().is_char_device());
        }
        let mut first = [0_u8; 8192];
        let mut second = [0_u8; 8192];
        fs::File::open("/dev/urandom")
            .unwrap()
            .read_exact(&mut first)
            .unwrap();
        fs::File::open("/dev/urandom")
            .unwrap()
            .read_exact(&mut second)
            .unwrap();
        assert_ne!(first, second);
        fs::write("/dev/null", b"discarded").unwrap();
        assert_eq!(fs::read("/dev/null").unwrap(), Vec::<u8>::new());
        assert_eq!(fs::metadata("/dev/null").unwrap().len(), 0);
        return;
    }

    let current = std::env::current_exe().unwrap();
    let (root, isolated) = root(&current);
    let executable = if isolated {
        Path::new("/bin/wrix")
    } else {
        &current
    };
    let name = std::thread::current().name().unwrap().to_owned();
    let output = command(
        root.path(),
        isolated,
        executable,
        &["--exact", &name, "--nocapture"],
    )
    .env(CHILD, "1")
    .output()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    if isolated {
        for device in ["dev/urandom", "dev/null"] {
            let path = root.path().join(device);
            assert!(fs::metadata(&path).unwrap().is_file());
            assert_eq!(fs::read(path).unwrap(), Vec::<u8>::new());
        }
        assert!(Path::new("/etc/wrix/image-agent").exists());
    }
}

pub fn install_executable(root: &Path, binary: &Path, destination: &Path) {
    let header = Command::new("readelf")
        .args(["-l"])
        .arg(binary)
        .output()
        .unwrap();
    assert!(header.status.success(), "readelf failed");
    let interpreter = String::from_utf8(header.stdout)
        .unwrap()
        .lines()
        .find_map(|line| {
            line.split_once("interpreter: ")
                .and_then(|(_, value)| value.strip_suffix(']'))
        })
        .map(PathBuf::from)
        .unwrap();
    let output = Command::new(&interpreter)
        .arg("--list")
        .arg(binary)
        .output()
        .unwrap();
    assert!(output.status.success(), "dynamic linker inspection failed");
    let mut paths: Vec<PathBuf> = String::from_utf8(output.stdout)
        .unwrap()
        .split_whitespace()
        .filter(|token| token.starts_with('/'))
        .map(PathBuf::from)
        .collect();
    paths.extend([binary.to_path_buf(), interpreter]);
    for path in paths {
        let target = root.join(
            if path == binary { destination } else { &path }
                .strip_prefix("/")
                .unwrap(),
        );
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        if !target.exists() {
            fs::copy(&path, target).unwrap();
        }
    }
}
