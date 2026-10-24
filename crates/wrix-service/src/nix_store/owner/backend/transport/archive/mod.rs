//! Bounded streaming to an already-reserved file; never buffer a NAR in controller memory.
use super::{Archive, read};
use std::{
    fs::File,
    io::{self, Write},
    process::{Command, Stdio},
};
use tokio::io::AsyncReadExt;

pub(super) fn capture(command: Command, file: File, limit: u64) -> io::Result<Archive> {
    std::thread::Builder::new()
        .name("wrix-image-export".into())
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async move {
                    let mut command = tokio::process::Command::from(command);
                    let mut child = command
                        .stdin(Stdio::null())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .kill_on_drop(true)
                        .spawn()?;
                    let mut stdout = child
                        .stdout
                        .take()
                        .ok_or_else(|| io::Error::other("missing NAR stream"))?;
                    let stderr = child
                        .stderr
                        .take()
                        .ok_or_else(|| io::Error::other("missing export diagnostic"))?;
                    let transfer = async move {
                        let mut output = file;
                        let mut buffer = vec![0_u8; 65536].into_boxed_slice();
                        let mut total = 0_u64;
                        loop {
                            let count = stdout.read(&mut buffer).await?;
                            if count == 0 {
                                break;
                            }
                            total = total
                                .checked_add(count as u64)
                                .ok_or_else(|| io::Error::other("NAR stream size overflow"))?;
                            if total > limit {
                                return Err(io::Error::other(
                                    "NAR stream exceeds reserved capacity",
                                ));
                            }
                            output.write_all(&buffer[..count])?;
                        }
                        output.flush()?;
                        Ok::<_, io::Error>(total)
                    };
                    let (status, bytes, stderr) =
                        tokio::try_join!(child.wait(), transfer, read(stderr))?;
                    Ok(Archive {
                        status,
                        bytes,
                        stderr,
                    })
                })
        })?
        .join()
        .map_err(|_| io::Error::other("image export helper thread failed"))?
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn streaming_export_honors_preallocated_bound_and_native_failure() {
        let file = tempfile::tempfile().unwrap();
        file.set_len(32).unwrap();
        let mut command = Command::new("sh");
        command.args(["-c", "printf archive; printf diagnostic >&2; exit 7"]);
        let output = capture(command, file.try_clone().unwrap(), 32).unwrap();
        assert_eq!(output.bytes, 7);
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stderr, b"diagnostic");
        assert_eq!(file.metadata().unwrap().len(), 32);
        let mut command = Command::new("sh");
        command.args(["-c", "head -c 33 /dev/zero"]);
        assert!(capture(command, file, 32).is_err());
    }
}
