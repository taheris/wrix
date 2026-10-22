//! Explicit host approval and fail-closed collection, without claiming an installed-store runtime.

pub mod gc;
pub mod native;

use std::{
    io::{self, Write},
    process::ExitCode,
};
use wrix_state::{context::Selection, host::Host, nix_store::Approval};

pub const HELP: &str = "Manage installed Nix store approval.\n\nUsage: wrix service nix-store [enable|disable|gc [--wait <duration>]]\n\nApproval explicitly binds the selected checked namespace to its canonical repository.\nEnable requires an existing host deployment; neither operation starts a pool.\nCollection never starts an owner; --wait bounds maintenance acquisition per pool (default 30s).\nZero wait attempts each available pool once without waiting.\nShared execution and collection remain unavailable in this version.\n";

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
            let report = gc::run(&gc::Unavailable, repository, wait).map_err(io::Error::other)?;
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
        }
    }
    writeln!(
        stdout,
        "installed-store runtime: unavailable; no pool has been provisioned by this interface"
    )?;
    Ok(ExitCode::SUCCESS)
}
