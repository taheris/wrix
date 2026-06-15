use std::{
    io::{self, Write},
    process::ExitCode,
};

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

    const fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Publish => "publish",
            Self::Warm => "warm",
            Self::Prune => "prune",
            Self::RotateKey => "rotate-key",
        }
    }
}

pub fn write_help(stdout: &mut impl Write) -> io::Result<()> {
    stdout.write_all(
        b"Manage the workspace project cache.\n\nUsage: wrix service cache <command>\n\nCommands:\n  status\n  publish\n  warm\n  prune\n  rotate-key\n",
    )
}

pub fn run(command: Command, stdout: &mut impl Write) -> io::Result<ExitCode> {
    writeln!(
        stdout,
        "wrix service cache {}: unavailable in this build",
        command.as_str()
    )?;
    Ok(ExitCode::SUCCESS)
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
