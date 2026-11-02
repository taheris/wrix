//! Disjoint fixture leases survive controller restarts without serializing whole tests.
use super::TestResult;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    net::{Ipv4Addr, TcpListener},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

#[derive(Deserialize, Serialize)]
struct Reservation {
    pid: u32,
    ports: [u16; 5],
}

fn alive(pid: u32) -> TestResult<bool> {
    let pid = rustix::process::Pid::from_raw(pid.try_into()?).ok_or("invalid fixture PID")?;
    match rustix::process::test_kill_process(pid) {
        Ok(()) | Err(rustix::io::Errno::PERM) => Ok(true),
        Err(rustix::io::Errno::SRCH) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn reserve() -> TestResult<[u16; 5]> {
    let root = std::env::temp_dir().canonicalize()?.join(format!(
        "wrix-test-role-ports-{}",
        rustix::process::geteuid().as_raw()
    ));
    wrix_core::private_directory::prepare_with_parents(&root)?;
    let mut ledger = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join("reservations.json"))?;
    FileExt::lock_exclusive(&ledger)?;
    let metadata = ledger.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.len() > 1024 * 1024
    {
        return Err("unsafe fixture port ledger".into());
    }
    let mut bytes = Vec::new();
    ledger.read_to_end(&mut bytes)?;
    let previous: Vec<Reservation> = if bytes.is_empty() {
        Vec::new()
    } else {
        serde_json::from_slice(&bytes)?
    };
    let mut reservations = Vec::new();
    let mut used = BTreeSet::new();
    for reservation in previous {
        if alive(reservation.pid)? {
            used.extend(reservation.ports);
            reservations.push(reservation);
        }
    }
    let mut listeners = Vec::new();
    let mut ports = [0; 5];
    // Upper halves avoid the production allocators' preferred low ports; the
    // other roles stay below the OS ephemeral-client range as well.
    for (slot, range) in ports.iter_mut().zip([
        24_000..25_000,
        22_000..23_000,
        26_000..45_000,
        26_000..45_000,
        24_000..25_000,
    ]) {
        for port in range {
            if used.contains(&port) {
                continue;
            }
            match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
                Ok(listener) => {
                    used.insert(port);
                    *slot = port;
                    listeners.push(listener);
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::AddrInUse => (),
                Err(error) => return Err(error.into()),
            }
        }
        if *slot == 0 {
            return Err("fixture role port range exhausted".into());
        }
    }
    assert_eq!(
        listeners.len(),
        ports.len(),
        "retain every probe listener through ledger publication"
    );
    reservations.push(Reservation {
        pid: std::process::id(),
        ports,
    });
    ledger.seek(SeekFrom::Start(0))?;
    ledger.set_len(0)?;
    ledger.write_all(&serde_json::to_vec(&reservations)?)?;
    ledger.sync_all()?;
    Ok(ports)
}

pub(super) fn prepare(authority: &Path) -> TestResult {
    wrix_core::private_directory::prepare_with_parents(authority)?;
    let path = authority.join("role-endpoints.json");
    if path.try_exists()? {
        return Ok(());
    }
    let ports = reserve()?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&serde_json::to_vec(
        &serde_json::json!({"version":2,"ports":ports}),
    )?)?;
    file.sync_all()?;
    File::open(authority)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn fixture_role_reservations_are_disjoint_even_after_probe_listeners_close() {
        let first = reserve().unwrap();
        let second = reserve().unwrap();
        assert!(first.iter().all(|port| !second.contains(port)));
    }

    #[tokio::test]
    async fn fixture_role_snapshot_is_accepted_unchanged_by_the_native_controller() {
        let owner = tempfile::tempdir().unwrap();
        let authority = owner
            .path()
            .canonicalize()
            .unwrap()
            .join("control/authority");
        prepare(&authority).unwrap();
        let before: serde_json::Value =
            serde_json::from_slice(&fs::read(authority.join("role-endpoints.json")).unwrap())
                .unwrap();
        let controller = wrix_service::control::Controller::bind(&authority).unwrap();
        let _host = wrix_service::control::serving::Host::bind_loopback(
            controller,
            std::num::NonZeroUsize::new(8).unwrap(),
        )
        .await
        .unwrap();
        let after: serde_json::Value =
            serde_json::from_slice(&fs::read(authority.join("role-endpoints.json")).unwrap())
                .unwrap();
        assert_eq!(after, before);
    }
}
