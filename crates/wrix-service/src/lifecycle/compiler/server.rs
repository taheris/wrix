//! Foreground native compiler owner and Rama forwarding, never linked into the client.

#[path = "startup/mod.rs"]
mod startup;
use crate::dolt::Error;
use std::{
    fs, io,
    path::Path,
    process::{Command, Stdio},
};
use tokio::process::Command as AsyncCommand;
pub use wrix_service::lifecycle::compiler::*;

async fn forward() -> Result<(), io::Error> {
    use rama_core::{Service, io::BridgeIo, rt::Executor};
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, 4226)).await?;
    let capacity = std::sync::Arc::new(tokio::sync::Semaphore::new(1024));
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let Ok(permit) = capacity.clone().try_acquire_owned() else { continue; };
                tasks.spawn(async move {
                    let _permit = permit;
                    match tokio::net::UnixStream::connect(SOCKET).await {
                        Ok(native) => {
                            let forward = rama_net::proxy::IoForwardService::new(Executor::default()).with_buf_size(16 * 1024);
                            if let Err(error) = forward.serve(BridgeIo(stream, native)).await { tracing::debug!(%error, "compiler native transport ended"); }
                        }
                        Err(error) => tracing::debug!(%error, "compiler native admission unavailable"),
                    }
                });
            }
            result = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(Err(error)) = result { return Err(io::Error::other(error)); }
            }
        }
    }
}

/// No native idle exit or detached daemon may outlive supervision or own the cache concurrently.
pub async fn run(
    data: &Path,
    transport: crate::dolt::server::transport::Transport,
) -> Result<(), Error> {
    let cache = data.parent().ok_or(Error::UnsafePath)?.join("compiler");
    crate::dolt::private_directory(&cache)?;
    let _startup = startup::prepare(&cache, Path::new("/run/wrix"))?;
    crate::dolt::private_directory(Path::new("/run/wrix/compiler"))?;
    crate::dolt::private_directory(Path::new("/run/wrix/celler"))?;
    crate::dolt::private_directory(Path::new("/run/wrix/compiler/execution/probe/workspace"))?;
    let mut command = Command::new("flock");
    command
        .args([
            "--exclusive",
            "--nonblock",
            "--conflict-exit-code",
            "75",
            "--no-fork",
        ])
        .arg(cache.join("owner.lock"))
        .arg("sccache");
    configure(&mut command, Path::new(SOCKET));
    command
        .env("SCCACHE_START_SERVER", "1")
        .env("SCCACHE_NO_DAEMON", "1")
        .env("SCCACHE_DIR", &cache)
        .env("WRIX_SCCACHE_DRAIN_SOCKET", "/run/wrix/compiler/drain.sock");
    let mut compiler = AsyncCommand::from(command)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let pid = compiler.id().ok_or(Error::Incomplete)?;
    fs::write(PID, format!("{pid}\n"))?;
    let backend = crate::dolt::server::run(data, transport);
    let forwarding = forward();
    tokio::pin!(backend, forwarding);
    let mut released = false;
    let result = loop {
        tokio::select! {
            result = &mut backend => break result,
            result = &mut forwarding => break result.map_err(Error::from),
            result = compiler.wait(), if !released => {
                let status = result?;
                if planned()? && status.success() {
                    record_release()?;
                    released = true;
                } else {
                    break Err(Error::Native { stderr: format!("supervised sccache exited unexpectedly: {status}") });
                }
            }
        }
    };
    if planned()? {
        if !released {
            let status = compiler.wait().await?;
            if !status.success() {
                return Err(Error::Incomplete);
            }
            record_release()?;
        }
    } else if compiler.try_wait()?.is_none() {
        let signalled = AsyncCommand::new("kill")
            .args(["-TERM", &pid.to_string()])
            .output()
            .await?;
        if !signalled.status.success() && compiler.try_wait()?.is_none() {
            return Err(Error::Incomplete);
        }
        compiler.wait().await?;
    }
    if planned()? && result.is_ok() {
        record_backend_release(data)?;
    }
    match fs::remove_file(PID) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    result
}
