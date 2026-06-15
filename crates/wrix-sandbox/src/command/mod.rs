use std::{
    io::{self, Write},
    process::ExitCode,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    Run,
    Spawn,
}

impl Command {
    pub fn parse(input: &str) -> Option<Self> {
        match input {
            "run" => Some(Self::Run),
            "spawn" => Some(Self::Spawn),
            _ => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Spawn => "spawn",
        }
    }
}

pub fn write_run_help(stdout: &mut impl Write) -> io::Result<()> {
    stdout.write_all(b"Run an interactive sandbox.\n\nUsage: wrix run [DIR] [CMD ...]\n")
}

pub fn write_spawn_help(stdout: &mut impl Write) -> io::Result<()> {
    stdout.write_all(
        b"Spawn a programmatic sandbox.\n\nUsage: wrix spawn --spawn-config <file> [--stdio]\n",
    )
}

pub fn run(command: Command, stdout: &mut impl Write) -> io::Result<ExitCode> {
    writeln!(
        stdout,
        "wrix {}: unavailable in this build",
        command.as_str()
    )?;
    Ok(ExitCode::SUCCESS)
}
