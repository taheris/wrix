//! Explicit host approval, recorded VM ownership and scoped native collection.

pub mod gc;
pub mod image;
pub mod lifecycle;
pub mod native;
pub mod owner;
pub mod worker;

use std::{
    io::{self, Write},
    process::ExitCode,
};
use wrix_state::{context::Selection, host::Host, nix_store::Approval};

pub const HELP: &str = "Manage installed Nix store approval.\n\nUsage: wrix service nix-store [enable|disable|gc [--wait <duration>]]\n\nApproval explicitly binds the selected checked namespace to its canonical repository.\nEnable requires an existing host deployment; neither operation starts a pool.\nCollection never starts an owner; --wait bounds maintenance acquisition per pool (default 30s).\nZero wait attempts each available pool once without waiting.\nCollection requires recorded running owners; shared launch delivery remains unavailable in this version.\n";

#[derive(Clone, Copy)]
enum Operation {
    Information,
    Enable,
    Disable,
    Collect(gc::Wait),
}

pub fn run(args: &[String], stdout: &mut impl Write) -> io::Result<ExitCode> {
    let operation = match args {
        [] => Operation::Information,
        [argument] if matches!(argument.as_str(), "--help" | "-h") => {
            stdout.write_all(HELP.as_bytes())?;
            return Ok(ExitCode::SUCCESS);
        }
        [command, argument] if command == "gc" && matches!(argument.as_str(), "--help" | "-h") => {
            stdout.write_all(HELP.as_bytes())?;
            return Ok(ExitCode::SUCCESS);
        }
        [argument] if argument == "enable" => Operation::Enable,
        [argument] if argument == "disable" => Operation::Disable,
        [argument] if argument == "gc" => Operation::Collect(gc::Wait::default()),
        [command, option, value] if command == "gc" && option == "--wait" => {
            Operation::Collect(gc::Wait::parse(value).map_err(io::Error::other)?)
        }
        _ => {
            return Err(io::Error::other(
                "invalid wrix service nix-store arguments; use --help",
            ));
        }
    };
    let host = Host::discover().map_err(io::Error::other)?;
    let selection = Selection::inspect(&std::env::current_dir()?).map_err(io::Error::other)?;
    let mut approval = wrix_state::nix_store::inspect(&selection).map_err(io::Error::other)?;
    match operation {
        Operation::Collect(wait) => {
            let Approval::Bound { repository, .. } = &approval else {
                return Err(io::Error::other(
                    "installed-store collection requires existing checked repository membership; no collection was dispatched",
                ));
            };
            selection.recheck().map_err(io::Error::other)?;
            let backend = gc::installed::Existing::open(&selection).map_err(|error| {
                io::Error::other(format!("installed-store collection unavailable: {error}"))
            })?;
            let report = gc::run(&backend, repository, wait).map_err(io::Error::other)?;
            for unit in &report.units {
                writeln!(stdout, "{}: {}", unit.pool.system.as_str(), unit.outcome)?;
            }
            return Ok(if report.complete() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            });
        }
        Operation::Information => (),
        Operation::Enable | Operation::Disable => {
            if matches!(operation, Operation::Enable) {
                selection.recheck().map_err(io::Error::other)?;
                if crate::control::existing_deployment(&host)
                    .map_err(io::Error::other)?
                    .is_none()
                {
                    return Err(io::Error::other(
                        "no recorded host deployment; run wrix service start before enabling installed-store sharing",
                    ));
                }
            }
            let context = selection
                .context()
                .ok_or_else(|| io::Error::other("checked host workspace authority is required"))?;
            wrix_state::nix_store::set(&host, context, matches!(operation, Operation::Enable))
                .map_err(io::Error::other)?;
            approval = wrix_state::nix_store::inspect(&selection).map_err(io::Error::other)?;
        }
    }
    match approval {
        Approval::Unavailable => writeln!(stdout, "installed-store approval: unavailable")?,
        Approval::Unapproved => writeln!(
            stdout,
            "installed-store approval: unapproved; namespace membership: unbound"
        )?,
        Approval::Bound {
            repository,
            enabled,
        } => {
            writeln!(
                stdout,
                "installed-store approval: {}; namespace membership: bound",
                if enabled { "enabled" } else { "disabled" }
            )?;
            writeln!(stdout, "canonical repository: {repository}")?;
            if matches!(operation, Operation::Information) {
                let deployment =
                    crate::control::existing_deployment(&host).map_err(io::Error::other)?;
                for (pool, phase) in owner::inspect(&host, &repository, deployment.as_ref())
                    .map_err(io::Error::other)?
                {
                    writeln!(
                        stdout,
                        "{}: host storage {}; native ownership/readiness not established",
                        pool.system.as_str(),
                        phase.as_str()
                    )?;
                    if let Some(deployment) = &deployment
                        && let Some(recorded) =
                            owner::backend::recorded_phase(&host, deployment, &pool)
                                .map_err(io::Error::other)?
                    {
                        writeln!(
                            stdout,
                            "{}: recorded native owner {}; readiness not probed",
                            pool.system.as_str(),
                            recorded.as_str()
                        )?;
                    }
                }
            }
        }
    }
    if matches!(operation, Operation::Information) {
        match image::Candidate::from_env() {
            Ok(candidate) => writeln!(
                stdout,
                "pool service image: {} (package-selected candidate)",
                candidate.image().as_str()
            )?,
            Err(image::Error::Missing) => writeln!(
                stdout,
                "pool service image: unavailable; no host-package binding"
            )?,
            Err(error) => return Err(io::Error::other(error)),
        }
    }
    writeln!(
        stdout,
        "installed-store worker delivery: unavailable; shared launches are not enabled"
    )?;
    Ok(ExitCode::SUCCESS)
}
