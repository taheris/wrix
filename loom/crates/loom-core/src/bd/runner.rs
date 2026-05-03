use std::ffi::OsString;
use std::future::Future;
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;

use super::error::BdError;

/// Captured stdout/stderr/exit code from a single `bd` invocation.
#[derive(Debug, Clone)]
pub struct RunOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl RunOutput {
    pub fn success(&self) -> bool {
        self.status == 0
    }
}

/// Subprocess execution boundary used by [`super::BdClient`].
///
/// Implementors run `bd <args>` (the program name is hardcoded by the
/// runner; only arguments arrive here) and surface the captured exit
/// code plus stdout/stderr. Tests substitute a capturing fake to avoid
/// a real `bd` binary; production code uses [`TokioRunner`].
pub trait CommandRunner: Send + Sync + 'static {
    fn run(
        &self,
        args: Vec<OsString>,
        timeout: Duration,
    ) -> impl Future<Output = Result<RunOutput, BdError>> + Send;
}

/// Default runner: `tokio::process::Command::new("bd")` with each argument
/// passed through `.arg()` so no shell is involved.
#[derive(Debug, Default, Clone, Copy)]
pub struct TokioRunner;

impl CommandRunner for TokioRunner {
    async fn run(&self, args: Vec<OsString>, t: Duration) -> Result<RunOutput, BdError> {
        let mut cmd = Command::new("bd");
        for arg in &args {
            cmd.arg(arg);
        }
        let fut = cmd.output();
        match timeout(t, fut).await {
            Ok(Ok(output)) => Ok(RunOutput {
                status: output.status.code().unwrap_or(-1),
                stdout: output.stdout,
                stderr: output.stderr,
            }),
            Ok(Err(e)) => Err(BdError::Spawn(e)),
            Err(_) => Err(BdError::Timeout {
                args: render_args(&args),
            }),
        }
    }
}

pub(super) fn render_args(args: &[OsString]) -> String {
    args.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}
