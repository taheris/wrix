//! Bounded runtime-exec transport. Cancellation never targets the serving backend or its work.

use super::{Error, Observation};
use std::{
    io,
    process::{Command, Stdio},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    time::timeout,
};

const LIMIT: usize = 4096;
const BUDGET: Duration = Duration::from_secs(15);

pub fn execute(command: Command) -> Result<Observation, Error> {
    std::thread::Builder::new()
        .name("wrix-component-transport".into())
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(capture(command, BUDGET))
        })?
        .join()
        .map_err(|_| Error::Worker)?
}

async fn bounded(mut reader: impl AsyncRead + Unpin) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    (&mut reader)
        .take(LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    Ok(bytes)
}

async fn capture(command: Command, budget: Duration) -> Result<Observation, Error> {
    let mut command = tokio::process::Command::from(command);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false)
        .spawn()?;
    let stdout = child.stdout.take().ok_or(Error::Disconnected)?;
    let stderr = child.stderr.take().ok_or(Error::Disconnected)?;
    let output = timeout(budget, async {
        tokio::try_join!(child.wait(), bounded(stdout), bounded(stderr))
    })
    .await;
    let (status, stdout, stderr) = if let Ok(output) = output {
        output?
    } else {
        // This is only our runtime CLI, not a guest role. An uncertain mutation remains fenced.
        child.start_kill()?;
        timeout(Duration::from_secs(1), child.wait())
            .await
            .map_err(|_| Error::Deadline)??;
        return Err(Error::Deadline);
    };
    if stdout.len() > LIMIT || stderr.len() > LIMIT {
        return Err(Error::Incompatible);
    }
    if !status.success() {
        return Err(Error::Transport {
            code: status.code(),
            diagnostic: String::from_utf8_lossy(&stderr).into_owned(),
        });
    }
    if !stderr.is_empty() {
        tracing::warn!(diagnostic = %String::from_utf8_lossy(&stderr), "component transport reported diagnostics");
    }
    Ok(serde_json::from_slice(&stdout)?)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn native_transport_requires_a_valid_completed_observation() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf '\"retired\"\\n'"]);
        assert_eq!(execute(command).unwrap(), Observation::Retired);
        let mut command = Command::new("sh");
        command.args(["-c", "printf 'failed' >&2; exit 7"]);
        assert!(matches!(
            execute(command),
            Err(Error::Transport { code: Some(7), .. })
        ));
    }

    #[tokio::test]
    async fn transport_deadline_stops_only_the_owned_helper() {
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 30"]);
        let start = std::time::Instant::now();
        assert!(matches!(
            capture(command, Duration::from_millis(30)).await,
            Err(Error::Deadline)
        ));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
