use std::{
    io::{self, Write},
    process::ExitCode,
};

use displaydoc::Display;
use thiserror::Error as ThisError;

use crate::publisher::{self, Mode};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Display, ThisError)]
pub enum Error {
    /// cache command I/O failed: {source}
    Io {
        #[from]
        source: io::Error,
    },
    /// {source}
    Publisher {
        #[from]
        source: publisher::Error,
    },
    /// unknown cache warm option: {option}
    UnknownWarmOption { option: String },
    /// unexpected cache option: {option}
    UnexpectedOption { option: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    Status,
    Publish,
    Warm,
    Prune,
    RotateKey,
}

impl Command {
    pub fn parse(input: &str) -> Option<Self> {
        match input {
            "status" => Some(Self::Status),
            "publish" => Some(Self::Publish),
            "warm" => Some(Self::Warm),
            "prune" => Some(Self::Prune),
            "rotate-key" => Some(Self::RotateKey),
            _ => None,
        }
    }
}

pub fn write_help(stdout: &mut impl Write) -> Result<()> {
    stdout.write_all(
        b"Manage the workspace project cache.\n\nUsage: wrix service cache <command> [options]\n\nCommands:\n  status\n  publish\n  warm [--checks]\n  prune\n  rotate-key\n",
    )?;
    Ok(())
}

pub fn run(command: Command, args: &[String], stdout: &mut impl Write) -> Result<ExitCode> {
    match command {
        Command::Status => run_status(args, stdout),
        Command::Publish => run_publish(args, stdout),
        Command::Warm => run_warm(args, stdout),
        Command::Prune => run_prune(args, stdout),
        Command::RotateKey => run_rotate_key(args, stdout),
    }
}

fn run_status(args: &[String], stdout: &mut impl Write) -> Result<ExitCode> {
    reject_args(args)?;
    let report = publisher::status_current_workspace()?;
    write_report(stdout, &report)?;
    Ok(ExitCode::SUCCESS)
}

fn run_publish(args: &[String], stdout: &mut impl Write) -> Result<ExitCode> {
    reject_args(args)?;
    let report = publisher::run_current_workspace(Mode::Publish)?;
    write_report(stdout, &report)?;
    Ok(ExitCode::SUCCESS)
}

fn run_warm(args: &[String], stdout: &mut impl Write) -> Result<ExitCode> {
    let checks = parse_warm_args(args)?;
    let report = publisher::run_current_workspace(Mode::Warm { checks })?;
    write_report(stdout, &report)?;
    Ok(ExitCode::SUCCESS)
}

fn run_prune(args: &[String], stdout: &mut impl Write) -> Result<ExitCode> {
    reject_args(args)?;
    let report = publisher::run_current_workspace(Mode::Prune)?;
    write_report(stdout, &report)?;
    Ok(ExitCode::SUCCESS)
}

fn run_rotate_key(args: &[String], stdout: &mut impl Write) -> Result<ExitCode> {
    reject_args(args)?;
    let report = publisher::run_current_workspace(Mode::RotateKey)?;
    write_report(stdout, &report)?;
    Ok(ExitCode::SUCCESS)
}

fn write_report(stdout: &mut impl Write, report: &publisher::Report) -> Result<()> {
    for line in report.lines() {
        writeln!(stdout, "{line}")?;
    }
    Ok(())
}

fn parse_warm_args(args: &[String]) -> Result<bool> {
    let mut checks = false;
    for arg in args {
        match arg.as_str() {
            "--checks" => checks = true,
            other => {
                return Err(Error::UnknownWarmOption {
                    option: other.to_owned(),
                });
            }
        }
    }
    Ok(checks)
}

fn reject_args(args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Ok(());
    }
    Err(Error::UnexpectedOption {
        option: args[0].clone(),
    })
}

#[cfg(test)]
mod test {
    use super::Command;

    #[test]
    fn cache_command_parser_accepts_public_cache_commands() {
        assert_eq!(Command::parse("status"), Some(Command::Status));
        assert_eq!(Command::parse("publish"), Some(Command::Publish));
        assert_eq!(Command::parse("warm"), Some(Command::Warm));
        assert_eq!(Command::parse("prune"), Some(Command::Prune));
        assert_eq!(Command::parse("rotate-key"), Some(Command::RotateKey));
    }
}
