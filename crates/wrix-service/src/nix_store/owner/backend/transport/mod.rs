//! Bounded helper I/O; cancelling a helper never proves native work or VM completion.

mod archive;

pub(super) struct Archive {
    pub status: std::process::ExitStatus,
    pub bytes: u64,
    pub stderr: Vec<u8>,
}
pub(super) fn export(
    command: std::process::Command,
    file: std::fs::File,
    limit: u64,
) -> std::io::Result<Archive> {
    archive::capture(command, file, limit)
}

use std::{
    io,
    process::{Command, Output, Stdio},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

const LIMIT: u64 = 4 * 1024 * 1024;

async fn read(reader: impl AsyncRead + Unpin) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(LIMIT + 1).read_to_end(&mut bytes).await?;
    if bytes.len() as u64 > LIMIT {
        return Err(io::Error::other("pool helper output exceeds its bound"));
    }
    Ok(bytes)
}

pub(super) fn capture(
    command: Command,
    input: Option<Vec<u8>>,
    budget: Option<Duration>,
) -> io::Result<Output> {
    std::thread::Builder::new()
        .name("wrix-pool-helper".into())
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async move {
                    let mut command = tokio::process::Command::from(command);
                    let mut child = command
                        .stdin(if input.is_some() {
                            Stdio::piped()
                        } else {
                            Stdio::null()
                        })
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .kill_on_drop(true)
                        .spawn()?;
                    let stdout = child
                        .stdout
                        .take()
                        .ok_or_else(|| io::Error::other("missing pool helper output"))?;
                    let stderr = child
                        .stderr
                        .take()
                        .ok_or_else(|| io::Error::other("missing pool helper diagnostic"))?;
                    let stdin = child.stdin.take();
                    let exchange = async {
                        let write = async move {
                            if let (Some(mut stdin), Some(input)) = (stdin, input) {
                                stdin.write_all(&input).await?;
                                stdin.shutdown().await?;
                            }
                            Ok::<_, io::Error>(())
                        };
                        let (status, stdout, stderr, ()) =
                            tokio::try_join!(child.wait(), read(stdout), read(stderr), write)?;
                        Ok::<_, io::Error>(Output {
                            status,
                            stdout,
                            stderr,
                        })
                    };
                    match budget {
                        Some(budget) => {
                            tokio::time::timeout(budget, exchange).await.map_err(|_| {
                                io::Error::new(
                                    io::ErrorKind::TimedOut,
                                    "pool helper deadline; native outcome remains uncertain",
                                )
                            })?
                        }
                        None => exchange.await,
                    }
                })
        })?
        .join()
        .map_err(|_| io::Error::other("pool helper thread failed"))?
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn helper_receives_eof_and_preserves_native_nonzero_status() {
        let mut command = Command::new("sh");
        command.args(["-c", "cat; exit 7"]);
        let output = capture(
            command,
            Some(b"request".to_vec()),
            Some(Duration::from_secs(2)),
        )
        .unwrap();
        assert_eq!(output.stdout, b"request");
        assert_eq!(output.status.code(), Some(7));
    }

    #[test]
    fn helper_deadline_does_not_wait_for_native_work() {
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 30"]);
        assert_eq!(
            capture(command, None, Some(Duration::from_millis(30)))
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[test]
    fn helper_output_is_bounded_without_an_execution_deadline() {
        let mut command = Command::new("sh");
        command.args(["-c", "exec head -c 4194305 /dev/zero"]);
        assert!(capture(command, None, None).is_err());
    }
}
