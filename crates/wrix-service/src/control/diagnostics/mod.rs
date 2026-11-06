//! Private, bounded last-failure diagnostics. Never readiness or ownership evidence.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

const FILE: &str = "last-controller-failure.json";
const LIMIT: u64 = 16_384;

fn private_root(root: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || root.canonicalize()? != root
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "invalid diagnostic root",
        ));
    }
    Ok(())
}

fn private_file(metadata: &fs::Metadata) -> bool {
    metadata.is_file()
        && metadata.uid() == rustix::process::geteuid().as_raw()
        && metadata.nlink() == 1
        && metadata.mode().trailing_zeros() >= 6
        && metadata.len() <= LIMIT
}

/// The supervised entry records only its rendered failure in its private control directory.
pub fn record_failure(root: &Path, error: &dyn std::fmt::Display) -> io::Result<()> {
    private_root(root)?;
    match fs::symlink_metadata(root.join(FILE)) {
        Ok(metadata) if !private_file(&metadata) => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "invalid diagnostic file",
            ));
        }
        Ok(_) => (),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error),
    }
    let detail: String = error.to_string().chars().take(2_048).collect();
    let data = serde_json::to_vec(&detail).map_err(io::Error::other)?;
    let mut staged = tempfile::NamedTempFile::new_in(root)?;
    staged.write_all(&data)?;
    staged.as_file().sync_all()?;
    staged
        .persist(root.join(FILE))
        .map_err(|error| error.error)?;
    File::open(root)?.sync_all()
}

fn last_failure(root: &Path) -> io::Result<String> {
    private_root(root)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join(FILE))?;
    if !private_file(&file.metadata()?) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "invalid diagnostic file",
        ));
    }
    let mut data = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut data)?;
    if data.len() as u64 > LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "oversized diagnostic",
        ));
    }
    serde_json::from_slice(&data).map_err(io::Error::other)
}

pub(super) fn explain(socket: &Path, error: super::Error) -> super::Error {
    if matches!(&error, super::Error::Io(source) if source.kind() == io::ErrorKind::UnexpectedEof)
        && let Some(root) = socket.parent().and_then(Path::parent)
        && let Ok(detail) = last_failure(root)
    {
        return super::Error::ControllerClosed { detail };
    }
    error
}

#[cfg(test)]
mod test {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn eof_reports_private_last_failure_without_claiming_readiness() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        record_failure(
            &root,
            &"guest listener 192.168.64.1:23001: Address already in use",
        )
        .unwrap();
        let error = explain(
            &root.join("authority/control.sock"),
            super::super::Error::Io(io::ErrorKind::UnexpectedEof.into()),
        );
        assert!(
            error
                .to_string()
                .contains("last recorded controller failure")
        );
        assert!(error.to_string().contains("192.168.64.1:23001"));
    }

    #[tokio::test]
    async fn authenticated_client_eof_includes_recorded_bind_failure() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let controller = super::super::Controller::bind(&root.join("authority")).unwrap();
        let client = super::super::Client::open(&root.join("authority")).unwrap();
        record_failure(
            &root,
            &"cannot bind vmnet guest listener 192.168.64.1:23001: Address already in use",
        )
        .unwrap();
        let served = async {
            let (mut stream, _) = controller.listener.accept().await.unwrap();
            let _: super::super::Request = super::super::receive(&mut stream).await.unwrap();
            drop(stream);
        };
        let ((), reply) = tokio::join!(served, client.request(super::super::Operation::Inspect {}));
        assert!(
            matches!(reply, Err(super::super::Error::ControllerClosed { detail }) if detail.contains("192.168.64.1:23001"))
        );
        controller.close().unwrap();
    }

    #[test]
    fn untrusted_diagnostic_permissions_and_symlinks_are_not_read() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        record_failure(&root, &"untrusted diagnostic").unwrap();
        fs::set_permissions(root.join(FILE), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(last_failure(&root).is_err());
        fs::remove_file(root.join(FILE)).unwrap();
        let target = root.join("foreign");
        fs::write(&target, b"\"foreign diagnostic\"").unwrap();
        std::os::unix::fs::symlink(&target, root.join(FILE)).unwrap();
        assert!(last_failure(&root).is_err());
        assert!(record_failure(&root, &"replacement").is_err());
        assert_eq!(fs::read(target).unwrap(), b"\"foreign diagnostic\"");
    }
}
