//! Fixed native backend work drain. Before release abort restores old admission; after release no old binary is restarted.

use crate::lifecycle::{
    Error, Result, Runtime, RuntimeKind, RuntimeStatus, compiler,
    drain::{self, Command as Action, Failure, Native, Phase, Response},
    owner,
};
use std::{
    io,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
enum Role {
    Dolt,
    Celler,
    Compiler,
}
impl Role {
    const ALL: [Self; 3] = [Self::Dolt, Self::Celler, Self::Compiler];
    const fn name(self) -> &'static str {
        match self {
            Self::Dolt => "dolt",
            Self::Celler => "celler",
            Self::Compiler => "compiler",
        }
    }
}
struct Client<'a> {
    runtime: &'a Runtime,
    selected: &'a owner::Selected,
    role: Role,
}
impl Native for Client<'_> {
    fn command(&mut self, action: Action) -> std::result::Result<Response, Failure> {
        let action_name = match action {
            Action::Begin => "begin",
            Action::Quiesce => "quiesce",
            Action::Status => "status",
            Action::Abort => "abort",
            Action::Release => "release",
        };
        let mut command = if action == Action::Release {
            Command::new(&self.runtime.binary)
        } else {
            let mut command = Command::new("timeout");
            command
                .args(["--signal=TERM", "--kill-after=1", "8"])
                .arg(&self.runtime.binary);
            command
        };
        command.args([
            "exec",
            self.selected.container.as_str(),
            "wrix-shared-service",
        ]);
        match self.role {
            Role::Dolt => {
                command.arg("sql-drain");
            }
            role => {
                command.args(["backend-drain", role.name()]);
            }
        }
        let output = command.arg(action_name).stdin(Stdio::null()).output()?;
        if !output.status.success() {
            return Err(io::Error::other(format!(
                "native {} {} command failed; completion is not inferred",
                self.role.name(),
                action_name
            ))
            .into());
        }
        if output.stdout.len() > 4096 {
            return Err(Failure::Oversize);
        }
        Ok(serde_json::from_slice(&output.stdout)?)
    }
}

pub(super) fn quiesce(runtime: &Runtime, selected: &owner::Selected) -> Result<()> {
    let result = (|| {
        for role in Role::ALL {
            drain::quiesce(
                &mut Client {
                    runtime,
                    selected,
                    role,
                },
                Duration::from_secs(30),
            )?;
        }
        for role in Role::ALL {
            let response = Client {
                runtime,
                selected,
                role,
            }
            .command(Action::Quiesce)?;
            if !response.accepted || !response.status.drained() {
                return Err(Failure::Uncertain);
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        abort(runtime, selected)?;
        return Err(Error::Operation {
            message: format!("native backend transition aborted before shutdown: {error}"),
        });
    }
    Ok(())
}

pub(super) fn abort(runtime: &Runtime, selected: &owner::Selected) -> Result<()> {
    let mut failed = Vec::new();
    for role in Role::ALL {
        match (Client {
            runtime,
            selected,
            role,
        })
        .command(Action::Abort)
        {
            Ok(response) if response.accepted && response.status.phase == Phase::Serving => (),
            Ok(_) => failed.push(format!(
                "{} admission was not confirmed serving",
                role.name()
            )),
            Err(error) => failed.push(format!("{}: {error}", role.name())),
        }
    }
    if !failed.is_empty() {
        return Err(Error::Operation {
            message: format!(
                "native pre-stop abort is incomplete; affected admission remains closed: {}",
                failed.join("; ")
            ),
        });
    }
    Ok(())
}

pub(super) fn release(runtime: &Runtime, selected: &owner::Selected, state: &Path) -> Result<()> {
    if runtime.status(&selected.container)? == RuntimeStatus::Running {
        let planned = Command::new(&runtime.binary)
            .args([
                "exec",
                selected.container.as_str(),
                "wrix-shared-service",
                "backend-planned-stop",
            ])
            .stdin(Stdio::null())
            .output()?;
        if !planned.status.success() {
            return Err(Error::Operation {
                message:
                    "native planned-stop authorization was not confirmed; no candidate was opened"
                        .into(),
            });
        }
        let compiler_done = Command::new(&runtime.binary)
            .args([
                "exec",
                selected.container.as_str(),
                "wrix-shared-service",
                "compiler-released",
            ])
            .stdin(Stdio::null())
            .output()?;
        if !compiler_done.status.success() {
            drain::release(&mut Client {
                runtime,
                selected,
                role: Role::Compiler,
            })
            .map_err(io::Error::other)?;
        }
        if let Err(error) = drain::release(&mut Client {
            runtime,
            selected,
            role: Role::Celler,
        }) && runtime.status(&selected.container)? == RuntimeStatus::Running
        {
            return Err(io::Error::other(error).into());
        }
    }
    clean_exit(runtime, selected, state)
}

fn clean_exit(runtime: &Runtime, selected: &owner::Selected, state: &Path) -> Result<()> {
    if runtime.kind == RuntimeKind::Podman {
        let output = Command::new(&runtime.binary)
            .args(["wait", selected.container.as_str()])
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() || output.stdout != b"0\n" {
            return Err(Error::Operation { message: "old native backend did not confirm zero exit; admission and checkpoint intent retained, no rollback".into() });
        }
    } else {
        let start = Instant::now();
        while runtime.status(&selected.container)? == RuntimeStatus::Running {
            if start.elapsed() >= Duration::from_secs(30) {
                return Err(Error::Operation { message: "native backend clean closure remains incomplete; no resource was forced out".into() });
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    if runtime.status(&selected.container)? != RuntimeStatus::Stopped {
        return Err(owner::Error::Conflict.into());
    }
    let boot = runtime
        .inspect_label(selected.container.as_str(), "wrix.service.boot")?
        .ok_or(owner::Error::Invalid)?;
    let boot = crate::gateway::Secret::try_from(boot).map_err(io::Error::other)?;
    let certificate = compiler::released_boot(&state.join("service-data/dolt"))?.ok_or_else(|| Error::Operation { message: "native supervisor did not reap all planned children at zero exit; release remains unconfirmed".into() })?;
    if !boot.matches(&certificate) {
        return Err(owner::Error::Conflict.into());
    }
    Ok(())
}
