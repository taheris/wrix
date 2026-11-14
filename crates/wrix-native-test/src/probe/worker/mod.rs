use std::{
    ffi::OsString,
    fs::OpenOptions,
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    thread,
    time::Duration,
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// unsupported private probe worker operation
    Usage,
    /// private probe worker I/O failed: {0}
    Io(#[from] io::Error),
    /// private probe worker identity could not be changed: {0}
    Identity(#[from] rustix::io::Errno),
    /// memory worker identity does not match the required unprivileged account
    Account,
    /// strict sandbox unexpectedly exposed the outside marker
    Marker,
    /// probe allocation could not be reserved
    Allocation,
}

#[cfg(target_os = "linux")]
fn root_memory() -> Result<(), Error> {
    barrier()?;
    if !rustix::process::getuid().is_root() {
        return Err(Error::Account);
    }
    if std::fs::read_dir("/proc/self/task")?
        .collect::<io::Result<Vec<_>>>()?
        .len()
        != 1
    {
        return Err(Error::Account);
    }
    rustix::thread::set_thread_groups(&[])?;
    rustix::thread::set_thread_gid(rustix::process::Gid::from_raw(30_000))?;
    rustix::thread::set_thread_uid(rustix::process::Uid::from_raw(30_001))?;
    memory(160 * 1024 * 1024)
}
fn barrier() -> Result<(), Error> {
    let mut byte = [0];
    io::stdin().read_exact(&mut byte)?;
    Ok(())
}
fn memory(bytes: usize) -> Result<(), Error> {
    if rustix::process::getuid().as_raw() != 30_001 {
        return Err(Error::Account);
    }
    let mut data = Vec::new();
    data.try_reserve_exact(bytes)
        .map_err(|_| Error::Allocation)?;
    data.resize(bytes, 1_u8);
    let data = std::hint::black_box(data);
    writeln!(io::stdout().lock(), "{}", data.len())?;
    Ok(())
}
fn strict(marker: &Path, out: &Path) -> Result<(), Error> {
    if !marker.is_absolute() || !out.is_absolute() {
        return Err(Error::Usage);
    }
    match std::fs::symlink_metadata(marker) {
        Ok(_) => return Err(Error::Marker),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let uid = rustix::process::getuid();
    if uid.is_root() {
        return Err(Error::Account);
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(out)?;
    writeln!(output, "strict sandbox build uid={}", uid.as_raw())?;
    output.sync_all()?;
    Ok(())
}
pub fn run(args: &[OsString]) -> Result<(), Error> {
    match args {
        [operation] if operation == "idle" => {
            barrier()?;
            thread::sleep(Duration::from_secs(90));
            Ok(())
        }
        #[cfg(target_os = "linux")]
        [operation] if operation == "root-memory" => root_memory(),
        [operation] if operation == "mapped-memory" => memory(128 * 1024 * 1024),
        [operation, marker] if operation == "strict-nix" => {
            let out = std::env::var_os("out").ok_or(Error::Usage)?;
            strict(Path::new(marker), Path::new(&out))
        }
        [operation, marker, out] if operation == "strict" => {
            strict(Path::new(marker), Path::new(out))
        }
        _ => Err(Error::Usage),
    }
}
#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn foreign_account_cannot_run_a_pressure_worker() {
        if rustix::process::getuid().as_raw() == 30_001 {
            memory(16).unwrap();
        } else {
            assert!(matches!(memory(16), Err(Error::Account)));
        }
    }
    #[test]
    fn strict_marker_presence_cannot_produce_an_output() {
        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        std::fs::write(root.join("marker"), b"outside").unwrap();
        assert!(matches!(
            strict(&root.join("marker"), &root.join("out")),
            Err(Error::Marker)
        ));
        assert!(!root.join("out").exists());
    }
    #[test]
    fn strict_worker_creates_only_the_requested_owned_output() {
        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        if rustix::process::getuid().is_root() {
            assert!(matches!(
                strict(&root.join("absent"), &root.join("out")),
                Err(Error::Account)
            ));
            assert!(!root.join("out").exists());
            return;
        }
        strict(&root.join("absent"), &root.join("out")).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("out")).unwrap(),
            format!(
                "strict sandbox build uid={}\n",
                rustix::process::getuid().as_raw()
            )
        );
        assert!(strict(&root.join("absent"), &root.join("out")).is_err());
    }
    #[test]
    fn unsupported_worker_mode_fails_before_reading_standard_input() {
        assert!(matches!(run(&["unknown".into()]), Err(Error::Usage)));
    }
}
