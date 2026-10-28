//! Run the production SQL gate on fixture-owned kernel descriptors, not invented readiness replies.
use super::TestResult;
use std::{
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    thread,
};

pub struct Server {
    pub socket: PathBuf,
    _root: tempfile::TempDir,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<thread::JoinHandle<io::Result<()>>>,
}
impl Server {
    pub fn start_at(root: &Path, data: &Path) -> TestResult<Self> {
        let directory = tempfile::Builder::new().prefix("sql-").tempdir_in(root)?;
        let socket = directory.path().join("control.sock");
        let local = directory.path().join("data.sock");
        let data = data.to_owned();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let (ready, started) = std::sync::mpsc::sync_channel(1);
        let control = socket.clone();
        let task = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
                let tcp = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
                let unix = tokio::net::UnixListener::bind(local)?;
                let control = tokio::net::UnixListener::bind(&control)?;
                fs::set_permissions(control.local_addr()?.as_pathname().ok_or_else(|| io::Error::other("missing native SQL fixture socket"))?, fs::Permissions::from_mode(0o600))?;
                ready.send(()).map_err(io::Error::other)?;
                tokio::select! {
                    result = wrix_service::lifecycle::sql::serve_on(&data, wrix_service::lifecycle::sql::Gate::default(), tcp, unix, control) => result,
                    _ = stopped => Ok(()),
                }
            })
        });
        let mut server = Self {
            socket,
            _root: directory,
            stop: Some(stop),
            task: Some(task),
        };
        if let Err(error) = started.recv_timeout(std::time::Duration::from_secs(10)) {
            server.stop.take();
            return Err(error.into());
        }
        Ok(server)
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take()
            && !matches!(task.join(), Ok(Ok(())))
        {
            tracing::error!("owned native SQL fixture did not stop cleanly");
        }
    }
}
