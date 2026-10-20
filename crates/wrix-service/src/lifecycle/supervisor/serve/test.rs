use super::*;
use crate::lifecycle::supervisor::client;
use std::{path::PathBuf, thread};
use wrix_core::path::Workspace;

struct Server {
    root: tempfile::TempDir,
    namespace: Namespace,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<Result<(), Error>>>,
}

impl Server {
    fn new(second: impl FnOnce(&Path) -> String) -> Result<Self, Error> {
        let root = tempfile::tempdir()?;
        let workspace = Workspace::from_path(root.path()).map_err(std::io::Error::other)?;
        let namespace = Namespace::from(workspace.hash());
        let first = format!(
            "echo $$ > '{}'; exec sleep 30",
            root.path().join("first.pid").display()
        );
        let second = second(root.path());
        let (stop, shutdown) = oneshot::channel();
        let path = root.path().to_path_buf();
        let identity = namespace.clone();
        let thread = thread::Builder::new().spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(run(
                    &path,
                    identity,
                    &path.join("dolt.sock"),
                    &first,
                    &second,
                    async {
                        if shutdown.await.is_err() {
                            tracing::debug!("test owner disconnected; stopping owned fixture");
                        }
                    },
                ))
        })?;
        Ok(Self {
            root,
            namespace,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    async fn ready(&self) -> Result<(), Error> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            match client::request(self.root.path(), &self.namespace, Action::Inspect).await {
                Ok(Observation::Active)
                    if self.root.path().join("first.pid").exists()
                        && self.root.path().join("launches").exists() =>
                {
                    return Ok(());
                }
                Ok(Observation::Active) => (),
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error),
                Ok(other) => {
                    return Err(std::io::Error::other(format!(
                        "unexpected component startup: {other:?}"
                    ))
                    .into());
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::Deadline);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn shutdown(&mut self) -> Result<(), Error> {
        fs::write(self.root.path().join("release"), "release owned fixture")?;
        if let Some(stop) = self.stop.take()
            && stop.send(()).is_err()
        {
            tracing::debug!("fixture supervisor already exited");
        }
        self.thread
            .take()
            .ok_or(Error::Disconnected)?
            .join()
            .map_err(|_| Error::Worker)?
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.thread.is_some()
            && let Err(error) = self.shutdown()
        {
            tracing::warn!(%error, "owned component fixture cleanup failed");
        }
    }
}

fn sleeper(root: &Path) -> String {
    format!(
        "echo $$ > '{}'; echo started >> '{}'; exec sleep 30",
        root.join("second.pid").display(),
        root.join("launches").display()
    )
}

#[tokio::test]
async fn planned_retirement_and_resume_keep_the_same_celler_process() {
    let mut server = Server::new(sleeper).unwrap();
    server.ready().await.unwrap();
    let first = fs::read(server.root.path().join("first.pid")).unwrap();
    client::retire(server.root.path(), &server.namespace)
        .await
        .unwrap();
    assert_eq!(
        client::request(server.root.path(), &server.namespace, Action::Inspect)
            .await
            .unwrap(),
        Observation::Retired
    );
    assert_eq!(
        fs::read(server.root.path().join("first.pid")).unwrap(),
        first
    );
    assert_eq!(
        client::request(server.root.path(), &server.namespace, Action::Resume)
            .await
            .unwrap(),
        Observation::Active
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while fs::read_to_string(server.root.path().join("launches"))
        .unwrap()
        .lines()
        .count()
        != 2
    {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    client::retire(server.root.path(), &server.namespace)
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(server.root.path().join("launches"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert_eq!(
        fs::read(server.root.path().join("first.pid")).unwrap(),
        first
    );
    server.shutdown().unwrap();
}

#[tokio::test]
async fn another_namespace_cannot_retire_or_resume_the_owned_component() {
    let mut server = Server::new(sleeper).unwrap();
    server.ready().await.unwrap();
    let other = tempfile::tempdir().unwrap();
    let namespace = Namespace::from(Workspace::from_path(other.path()).unwrap().hash());
    for action in [Action::Retire, Action::Resume] {
        assert!(matches!(
            client::request(server.root.path(), &namespace, action).await,
            Err(Error::Incompatible)
        ));
    }
    assert_eq!(
        client::request(server.root.path(), &server.namespace, Action::Inspect)
            .await
            .unwrap(),
        Observation::Active
    );
    server.shutdown().unwrap();
}

#[tokio::test]
async fn unexpected_celler_exit_stops_dolt_and_fails_supervision() {
    let mut server = Server::new(sleeper).unwrap();
    server.ready().await.unwrap();
    let pid = fs::read_to_string(server.root.path().join("first.pid")).unwrap();
    assert!(
        NativeCommand::new("kill")
            .args(["-TERM", pid.trim()])
            .status()
            .unwrap()
            .success()
    );
    let result = server.thread.take().unwrap().join().unwrap();
    assert!(matches!(
        result,
        Err(Error::Exited {
            role: Role::Celler,
            ..
        })
    ));
    assert!(Held::acquire(server.root.path()).unwrap().is_some());
}

#[tokio::test]
async fn unexpected_dolt_exit_stops_celler_and_fails_supervision() {
    let mut server = Server::new(|root| {
        format!(
            "echo started > '{}'; while test ! -f '{}'; do sleep 0.01; done; exit 23",
            root.join("launches").display(),
            root.join("fail").display(),
        )
    })
    .unwrap();
    server.ready().await.unwrap();
    let pid = fs::read_to_string(server.root.path().join("first.pid")).unwrap();
    fs::write(server.root.path().join("fail"), "fail owned fixture").unwrap();
    let result = server.thread.take().unwrap().join().unwrap();
    assert!(matches!(
        result,
        Err(Error::Exited {
            role: Role::Dolt,
            code: Some(23)
        })
    ));
    assert!(
        !NativeCommand::new("kill")
            .args(["-0", pid.trim()])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn stale_socket_cleanup_preserves_non_socket_and_symlink_targets() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("dolt.sock");
    let uid = fs::metadata(root.path()).unwrap().uid();
    fs::write(&path, "preserve").unwrap();
    assert!(matches!(
        remove_stale_socket(&path, uid),
        Err(Error::Permissions)
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), "preserve");
    fs::remove_file(&path).unwrap();
    let target = root.path().join("target");
    fs::write(&target, "preserve target").unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(matches!(
        remove_stale_socket(&path, uid),
        Err(Error::Permissions)
    ));
    assert_eq!(fs::read_to_string(target).unwrap(), "preserve target");
    fs::remove_file(&path).unwrap();
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
    remove_stale_socket(&path, uid).unwrap();
    assert!(!path.exists());
}

#[tokio::test]
async fn guard_probe_failure_stops_owned_celler_without_clearing_uncertainty() {
    let mut server = Server::new(sleeper).unwrap();
    server.ready().await.unwrap();
    let pid = fs::read_to_string(server.root.path().join("first.pid")).unwrap();
    fs::set_permissions(
        server.root.path().join("dolt-epoch.lock"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let retirement = client::request(server.root.path(), &server.namespace, Action::Retire).await;
    assert!(matches!(
        retirement,
        Ok(Observation::Retiring) | Err(Error::Io(_))
    ));
    let result = server.thread.take().unwrap().join().unwrap();
    let alive = NativeCommand::new("kill")
        .args(["-0", pid.trim()])
        .output()
        .unwrap()
        .status
        .success();
    if alive {
        assert!(
            NativeCommand::new("kill")
                .args(["-TERM", pid.trim()])
                .status()
                .unwrap()
                .success()
        );
    }
    assert!(
        !alive,
        "guard probe failure orphaned the owned Celler process"
    );
    assert!(matches!(result, Err(Error::Cleanup { source, cleanup })
        if matches!(*source, Error::Permissions) && matches!(*cleanup, Error::Permissions)));
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(server.root.path().join("component.json")).unwrap())
            .unwrap();
    assert_eq!(record["epoch"], "serving");
}

#[tokio::test]
async fn native_descendants_hold_retirement_busy_after_the_main_child_exits() {
    let mut server = Server::new(|root| {
        format!(
            "echo started > '{}'; (trap '' TERM; while test ! -f '{}'; do sleep 0.02; done) & echo ready > '{}'; wait",
            root.join("launches").display(),
            root.join("release").display(),
            root.join("descendant").display(),
        )
    }).unwrap();
    server.ready().await.unwrap();
    let marker: PathBuf = server.root.path().join("descendant");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while !marker.exists() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        client::request(server.root.path(), &server.namespace, Action::Retire)
            .await
            .unwrap(),
        Observation::Retiring
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        client::request(server.root.path(), &server.namespace, Action::Inspect)
            .await
            .unwrap(),
        Observation::Retiring
    );
    assert_eq!(
        client::request(server.root.path(), &server.namespace, Action::Resume)
            .await
            .unwrap(),
        Observation::Busy
    );
    fs::write(
        server.root.path().join("release"),
        "complete native fixture work",
    )
    .unwrap();
    client::retire(server.root.path(), &server.namespace)
        .await
        .unwrap();
    server.shutdown().unwrap();
}
