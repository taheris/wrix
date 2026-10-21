//! Execute unmodified host binaries across an actual filesystem isolation boundary.
#![cfg(test)]
use std::{
    fs,
    io::Read,
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
        let mut random = [0_u8; 4096];
        fs::File::open("/dev/urandom")
            .unwrap()
            .read_exact(&mut random)
            .unwrap();
        fs::write(root.path().join("dev/urandom"), random).unwrap();
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
        let mut command = Command::new("chroot");
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
