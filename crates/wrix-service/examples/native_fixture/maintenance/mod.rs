use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    net::Shutdown,
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command, ExitCode, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// unsupported maintenance fixture arguments
    Usage,
    /// maintenance fixture I/O failed: {0}
    Io(#[from] io::Error),
    /// maintenance fixture JSON failed: {0}
    Json(#[from] serde_json::Error),
    /// could not terminate the owned maintenance child
    Signal,
    /// owned maintenance child did not stop within ten seconds
    Timeout,
}

#[derive(Debug, Serialize, Deserialize)]
struct Reply {
    code: u8,
    stdout: String,
    stderr: String,
}

fn read_bounded(reader: &mut impl Read, limit: u64) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    BufReader::new(reader.take(limit + 1)).read_until(b'\n', &mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "oversized fixture request/reply",
        )
        .into());
    }
    Ok(bytes)
}

fn request(path: &Path, args: &[OsString]) -> Result<Reply, Error> {
    let args = args
        .iter()
        .map(|arg| arg.to_str().ok_or(Error::Usage))
        .collect::<Result<Vec<_>, _>>()?;
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    serde_json::to_writer(&mut stream, &args)?;
    stream.write_all(b"\n")?;
    stream.shutdown(Shutdown::Write)?;
    Ok(serde_json::from_slice(&read_bounded(
        &mut stream,
        1024 * 1024,
    )?)?)
}

struct Owner(Option<Child>);

impl Owner {
    fn stop(&mut self) -> Result<(), Error> {
        if let Some(child) = self.0.as_mut() {
            if child.try_wait()?.is_none() {
                if !Command::new("kill")
                    .args(["-TERM", &child.id().to_string()])
                    .status()?
                    .success()
                {
                    return Err(Error::Signal);
                }
                let deadline = Instant::now() + Duration::from_secs(10);
                while child.try_wait()?.is_none() {
                    if Instant::now() >= deadline {
                        return Err(Error::Timeout);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            child.wait()?;
        }
        self.0 = None;
        Ok(())
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            tracing::error!(%error, "owned maintenance fixture cleanup failed");
        }
    }
}

fn launch(program: &Path, args: &[&Path], diagnostics: PathBuf) -> Result<Child, Error> {
    Ok(Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(File::create(diagnostics)?)
        .spawn()?)
}

fn dispatch(
    root: &Path,
    executable: &Path,
    child: &mut Owner,
    args: &[String],
) -> Result<Reply, Error> {
    let mut reply = Reply {
        code: 0,
        stdout: String::new(),
        stderr: String::new(),
    };
    let words: Vec<_> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["--user", "show-environment" | "daemon-reload"] => {}
        ["--user", "show", _, _] => {
            reply.stdout = format!(
                "LoadState=loaded\nActiveState=active\nFragmentPath={}\n",
                root.join("user-units/wrix-host-controller.service")
                    .display()
            );
        }
        ["--user", "stop", "wrix-host-controller.service"] => child.stop()?,
        ["--user", "start", "wrix-host-controller.service"] => {
            if child.0.is_none() {
                child.0 = Some(launch(
                    executable,
                    &[root],
                    root.join("new-controller.stderr"),
                )?);
            }
        }
        ["--user", "link", path]
            if Path::new(path) == root.join("wrix-host-controller.service") =>
        {
            let linked = root.join("user-units/wrix-host-controller.service");
            fs::create_dir_all(root.join("user-units"))?;
            match fs::remove_file(&linked) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            std::os::unix::fs::symlink(path, linked)?;
        }
        _ => {
            reply.code = 91;
            reply.stderr = format!("unexpected fixture supervisor request: {args:?}");
        }
    }
    Ok(reply)
}

async fn serve(root: &Path, executable: &Path) -> Result<(), Error> {
    let mut termination =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut child = Owner(Some(launch(
        &std::env::current_exe()?,
        &[Path::new("maintenance"), Path::new("legacy"), root],
        root.join("legacy.stderr"),
    )?));
    let listener = UnixListener::bind(root.join("manager.sock"))?;
    listener.set_nonblocking(true)?;
    loop {
        tokio::select! {
            _ = termination.recv() => break,
            () = tokio::time::sleep(Duration::from_millis(10)) => {},
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_nonblocking(false)?;
                stream.set_read_timeout(Some(Duration::from_secs(10)))?;
                let args: Vec<String> = serde_json::from_slice(&read_bounded(&mut stream, 8192)?)?;
                let mut log = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(root.join("manager.calls"))?;
                serde_json::to_writer(&mut log, &args)?;
                log.write_all(b"\n")?;
                serde_json::to_writer(
                    &mut stream,
                    &dispatch(root, executable, &mut child, &args)?,
                )?;
                stream.write_all(b"\n")?;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.into()),
        }
    }
    child.stop()
}

pub(super) fn run(args: &[OsString]) -> Result<ExitCode, Error> {
    match args {
        [mode, root] if mode == "legacy" => {
            let root = Path::new(root);
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .open(root.join("authority/ownership"))?;
            lock.try_lock_exclusive()?;
            File::create(root.join("legacy-ready"))?;
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        [mode, path, args @ ..] if mode == "request" => {
            let reply = request(Path::new(path), args)?;
            io::stdout().lock().write_all(reply.stdout.as_bytes())?;
            io::stderr().lock().write_all(reply.stderr.as_bytes())?;
            Ok(ExitCode::from(reply.code))
        }
        [mode, root, executable] if mode == "serve" => {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(serve(Path::new(root), Path::new(executable)))?;
            Ok(ExitCode::SUCCESS)
        }
        _ => Err(Error::Usage),
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn supervisor_links_unit_and_reports_active_fragment_without_starting_a_child() {
        let root = tempfile::tempdir().unwrap();
        let mut child = Owner(None);
        let unit = root.path().join("wrix-host-controller.service");
        fs::write(&unit, "[Service]\n").unwrap();
        let args = ["--user", "link", unit.to_str().unwrap()].map(String::from);
        assert_eq!(
            dispatch(root.path(), Path::new("unused"), &mut child, &args)
                .unwrap()
                .code,
            0
        );
        assert_eq!(
            fs::read_link(root.path().join("user-units/wrix-host-controller.service")).unwrap(),
            unit
        );
        let args = [
            "--user",
            "show",
            "wrix-host-controller.service",
            "--no-pager",
        ]
        .map(String::from);
        let reply = dispatch(root.path(), Path::new("unused"), &mut child, &args).unwrap();
        assert!(
            reply
                .stdout
                .contains("LoadState=loaded\nActiveState=active\nFragmentPath=")
        );
        assert!(
            reply
                .stdout
                .contains(unit.file_name().unwrap().to_str().unwrap())
        );
        assert!(child.0.is_none());
    }

    #[test]
    fn supervisor_rejects_unexpected_mutation_with_original_exit_code() {
        let root = tempfile::tempdir().unwrap();
        let args = ["--user", "restart", "other.service"].map(String::from);
        let reply = dispatch(root.path(), Path::new("unused"), &mut Owner(None), &args).unwrap();
        assert_eq!(reply.code, 91);
        assert!(
            reply
                .stderr
                .contains("unexpected fixture supervisor request")
        );
    }

    #[test]
    fn socket_client_preserves_arguments_streams_and_failure_status() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("manager.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let (release, released) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let args: Vec<String> =
                serde_json::from_slice(&read_bounded(&mut stream, 8192).unwrap()).unwrap();
            assert_eq!(args, ["two words", "", "λ", "two\nlines"]);
            serde_json::to_writer(
                &mut stream,
                &Reply {
                    code: 91,
                    stdout: "output λ".into(),
                    stderr: "diagnostic\n".into(),
                },
            )
            .unwrap();
            stream.write_all(b"\n").unwrap();
            released.recv_timeout(Duration::from_secs(20)).unwrap();
        });
        let reply = request(
            &path,
            &["two words", "", "λ", "two\nlines"].map(OsString::from),
        )
        .unwrap();
        assert_eq!(reply.code, 91);
        assert_eq!(reply.stdout, "output λ");
        assert_eq!(reply.stderr, "diagnostic\n");
        release.send(()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn supervisor_starts_one_native_child_and_waits_for_graceful_stop() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("controller");
        fs::write(&executable, "#!/usr/bin/env bash\nset -euo pipefail\ntrap 'printf stopped > \"$1/stopped\"; exit 0' TERM\nprintf ready > \"$1/ready\"\nwhile :; do sleep 0.01; done\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let mut child = Owner(None);
        let start = ["--user", "start", "wrix-host-controller.service"].map(String::from);
        dispatch(root.path(), &executable, &mut child, &start).unwrap();
        let pid = child.0.as_ref().unwrap().id();
        dispatch(root.path(), &executable, &mut child, &start).unwrap();
        assert_eq!(child.0.as_ref().unwrap().id(), pid);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !root.path().join("ready").exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        let stop = ["--user", "stop", "wrix-host-controller.service"].map(String::from);
        dispatch(root.path(), &executable, &mut child, &stop).unwrap();
        assert!(child.0.is_none());
        assert_eq!(fs::read(root.path().join("stopped")).unwrap(), b"stopped");
        dispatch(root.path(), &executable, &mut child, &stop).unwrap();
    }

    #[test]
    fn socket_frame_completes_without_peer_write_shutdown() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        writer.write_all(b"[]\n").unwrap();
        assert_eq!(read_bounded(&mut reader, 8192).unwrap(), b"[]\n");
    }

    #[test]
    fn socket_payload_limit_rejects_oversized_input() {
        assert!(read_bounded(&mut &b"12345"[..], 4).is_err());
        assert_eq!(read_bounded(&mut &b"1234"[..], 4).unwrap(), b"1234");
        assert!(read_bounded(&mut &b"1234\n"[..], 4).is_err());
        assert_eq!(read_bounded(&mut &b"123\n"[..], 4).unwrap(), b"123\n");
    }
}
