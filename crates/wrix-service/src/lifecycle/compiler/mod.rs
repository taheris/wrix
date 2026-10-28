//! A foreground native sccache owner shares the backend lifetime; probes exercise actual native compilation.

mod startup;

use crate::dolt::Error;
use std::{
    fs, io,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
    process::{Command, Stdio},
};
use tokio::process::Command as AsyncCommand;

const SOCKET: &str = "/run/wrix/compiler/sccache.sock";
const PID: &str = "/run/wrix/compiler/pid";

fn configure(command: &mut Command, socket: &Path) {
    command
        .env("SCCACHE_SERVER_UDS", socket)
        .env("SCCACHE_IDLE_TIMEOUT", "0")
        .env("SCCACHE_CACHE_SIZE", "50G")
        .env("WRIX_SCCACHE_MANAGED", "1")
        .env("WRIX_COMPILER_EXECUTION", "probe")
        .env_remove("SCCACHE_START_SERVER")
        .env_remove("SCCACHE_NO_DAEMON")
        .env_remove("SCCACHE_SERVER_PORT")
        .env_remove("SCCACHE_ENDPOINT")
        .env_remove("SCCACHE_BUCKET")
        .env_remove("SCCACHE_REDIS")
        .env_remove("SCCACHE_MEMCACHED")
        .env_remove("SCCACHE_GHA_ENABLED");
}

fn checked(command: &mut Command) -> Result<Vec<u8>, Error> {
    let output = command.stdin(Stdio::null()).output()?;
    if !output.status.success() {
        return Err(Error::Native {
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(output.stdout)
}

/// Success requires a supervised native process, its Unix protocol and a compiled executable.
/// An installed sccache binary or a quiet transport is not sufficient evidence.
pub fn ready() -> Result<(), Error> {
    ready_at(
        Path::new(SOCKET),
        Path::new(PID),
        Path::new("/run/wrix/compiler/execution"),
        Path::new("sccache"),
    )
}

fn ready_at(socket: &Path, pid: &Path, views: &Path, compiler: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(socket)?;
    if !metadata.file_type().is_socket() || metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(Error::Incomplete);
    }
    let pid = fs::read_to_string(pid)?;
    let pid: u32 = pid.trim().parse().map_err(|_| Error::Incomplete)?;
    if pid == 0 {
        return Err(Error::Incomplete);
    }
    checked(Command::new("kill").args(["-0", &pid.to_string()]))?;
    let workspace = views.join("probe/workspace");
    crate::dolt::private_directory(&workspace)?;
    if workspace.canonicalize()? != workspace {
        return Err(Error::UnsafePath);
    }
    let directory = tempfile::tempdir_in(&workspace)?;
    let source = directory.path().join("probe.rs");
    let executable = directory.path().join("probe");
    fs::write(
        &source,
        "pub fn answer() -> u64 { 7_u64.wrapping_mul(9) }\n",
    )?;
    let mut compile = Command::new("timeout");
    compile
        .args(["--signal=TERM", "20"])
        .arg(compiler)
        .args([
            "rustc",
            "--crate-name",
            "wrix_readiness",
            "--crate-type=lib",
            "--emit=link",
            "--out-dir",
        ])
        .arg(directory.path())
        .arg(&source);
    configure(&mut compile, socket);
    compile.current_dir(directory.path());
    checked(&mut compile)?;
    let main = directory.path().join("main.rs");
    fs::write(
        &main,
        "extern crate wrix_readiness; fn main() { assert_eq!(wrix_readiness::answer(), 63); }\n",
    )?;
    checked(
        Command::new("rustc")
            .arg("--extern")
            .arg(format!(
                "wrix_readiness={}",
                directory.path().join("libwrix_readiness.rlib").display()
            ))
            .arg("-o")
            .arg(&executable)
            .arg(main),
    )?;
    checked(&mut Command::new(&executable))?;
    let mut stats = Command::new(compiler);
    stats.args(["--show-stats", "--stats-format=json"]);
    configure(&mut stats, socket);
    let stats: serde_json::Value =
        serde_json::from_slice(&checked(&mut stats)?).map_err(|_| Error::Incomplete)?;
    if stats["stats"]["requests_executed"]
        .as_u64()
        .is_none_or(|requests| requests == 0)
        || stats["max_cache_size"].as_u64() != Some(50 * 1024 * 1024 * 1024)
    {
        return Err(Error::Incomplete);
    }
    let mut management = Command::new(compiler);
    management.args(["--wrix-work-drain", "status"]).env(
        "WRIX_SCCACHE_DRAIN_SOCKET",
        socket.parent().ok_or(Error::UnsafePath)?.join("drain.sock"),
    );
    let response: super::drain::Response =
        serde_json::from_slice(&checked(&mut management)?).map_err(|_| Error::Incomplete)?;
    if !response.accepted
        || response.status.phase != super::drain::Phase::Serving
        || response.status.failed
    {
        return Err(Error::Incomplete);
    }
    checked(Command::new("kill").args(["-0", &pid.to_string()]))?;
    Ok(())
}

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

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct BackendRelease {
    version: u16,
    boot: crate::gateway::Secret,
}

pub fn clear_backend_release(data: &Path) -> Result<(), Error> {
    match fs::remove_file(data.join("native-release.json")) {
        Ok(()) => fs::File::open(data)?.sync_all()?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn record_backend_release(data: &Path) -> Result<(), Error> {
    let boot = crate::gateway::Secret::try_from(
        std::env::var("WRIX_BACKEND_BOOT").map_err(|_| Error::Incomplete)?,
    )
    .map_err(|_| Error::Incomplete)?;
    let mut file = tempfile::NamedTempFile::new_in(data)?;
    std::io::Write::write_all(
        &mut file,
        &serde_json::to_vec(&BackendRelease { version: 1, boot }).map_err(|_| Error::Incomplete)?,
    )?;
    file.as_file().sync_all()?;
    file.persist(data.join("native-release.json"))
        .map_err(|error| error.error)?;
    fs::File::open(data)?.sync_all()?;
    Ok(())
}

pub(in crate::lifecycle) fn released_boot(
    data: &Path,
) -> Result<Option<crate::gateway::Secret>, Error> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    let file = match fs::File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(data.join("native-release.json"))
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != fs::metadata(data)?.uid()
        || metadata.len() > 1024
    {
        return Err(Error::UnsafePath);
    }
    let mut bytes = Vec::new();
    file.take(1025).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 {
        return Err(Error::Incomplete);
    }
    let release: BackendRelease = serde_json::from_slice(&bytes).map_err(|_| Error::Incomplete)?;
    if release.version != 1 {
        return Err(Error::Incomplete);
    }
    Ok(Some(release.boot))
}

pub fn planned() -> Result<bool, Error> {
    Ok(Path::new("/run/wrix/backend/planned-stop").try_exists()?)
}

pub fn plan_stop() -> Result<(), Error> {
    crate::dolt::private_directory(Path::new("/run/wrix/backend"))?;
    let mut file = tempfile::NamedTempFile::new_in("/run/wrix/backend")?;
    std::io::Write::write_all(
        &mut file,
        b"native backend work drained; clean role release authorized\n",
    )?;
    file.as_file().sync_all()?;
    file.persist("/run/wrix/backend/planned-stop")
        .map_err(|error| error.error)?;
    fs::File::open("/run/wrix/backend")?.sync_all()?;
    Ok(())
}

fn record_release() -> Result<(), Error> {
    let mut file = tempfile::NamedTempFile::new_in("/run/wrix/compiler")?;
    std::io::Write::write_all(&mut file, b"0\n")?;
    file.as_file().sync_all()?;
    file.persist("/run/wrix/compiler/released")
        .map_err(|error| error.error)?;
    fs::File::open("/run/wrix/compiler")?.sync_all()?;
    Ok(())
}

pub fn released() -> Result<bool, Error> {
    match fs::read("/run/wrix/compiler/released") {
        Ok(bytes) => Ok(bytes == b"0\n"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// No native idle exit or detached daemon may outlive supervision or own the cache concurrently.
pub async fn run(data: &Path, tcp: bool) -> Result<(), Error> {
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
    let backend = crate::dolt::server::run(data, tcp);
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

#[cfg(test)]
mod test {
    use super::*;
    use std::time::{Duration, Instant};

    struct Process(std::process::Child);
    impl Drop for Process {
        fn drop(&mut self) {
            if self.0.try_wait().unwrap().is_none() {
                self.0.kill().unwrap();
                self.0.wait().unwrap();
            }
        }
    }

    #[test]
    #[ignore = "requires WRIX_TEST_SCCACHE pointing to the actual patched native daemon, flock and rustc"]
    fn native_compiler_readiness_executes_daemon_work_and_rejects_stopped_owners() {
        let binary = std::path::PathBuf::from(
            std::env::var_os("WRIX_TEST_SCCACHE").expect("patched native sccache required"),
        );
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("compiler.sock");
        let pid = root.path().join("pid");
        let mut start = Command::new("flock");
        start
            .args(["--exclusive", "--nonblock", "--no-fork"])
            .arg(root.path().join("owner.lock"))
            .arg(&binary);
        configure(&mut start, &socket);
        start
            .env("SCCACHE_START_SERVER", "1")
            .env("SCCACHE_NO_DAEMON", "1")
            .env("SCCACHE_DIR", root.path().join("cache"))
            .env("WRIX_COMPILER_VIEW_ROOT", root.path().join("execution"))
            .env("WRIX_SCCACHE_DRAIN_SOCKET", root.path().join("drain.sock"));
        let mut process = Process(start.spawn().unwrap());
        fs::write(&pid, process.0.id().to_string()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !socket.exists() {
            assert!(
                process.0.try_wait().unwrap().is_none(),
                "native sccache exited before readiness"
            );
            assert!(
                Instant::now() < deadline,
                "native sccache listener did not appear"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        ready_at(&socket, &pid, &root.path().join("execution"), &binary).unwrap();
        for action in ["begin", "release"] {
            checked(
                Command::new(&binary)
                    .args(["--wrix-work-drain", action])
                    .env("WRIX_SCCACHE_DRAIN_SOCKET", root.path().join("drain.sock")),
            )
            .unwrap();
        }
        assert!(process.0.wait().unwrap().success());
        assert!(ready_at(&socket, &pid, &root.path().join("execution"), &binary).is_err());
    }

    #[test]
    fn compiler_readiness_rejects_a_regular_file_before_starting_any_daemon() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("socket");
        fs::write(&socket, "not a native listener").unwrap();
        assert!(matches!(
            ready_at(
                &socket,
                &root.path().join("pid"),
                &root.path().join("execution"),
                Path::new("sccache")
            ),
            Err(Error::Incomplete)
        ));
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
