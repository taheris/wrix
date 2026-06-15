use std::{
    io::{self, Write},
    process::ExitCode,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    Push,
}

impl Command {
    pub fn parse(input: &str) -> Option<Self> {
        match input {
            "push" => Some(Self::Push),
            _ => None,
        }
    }
}

pub fn write_help(stdout: &mut impl Write) -> io::Result<()> {
    stdout
        .write_all(b"Manage beads workflows.\n\nUsage: wrix beads <command>\n\nCommands:\n  push\n")
}

pub fn run(command: Command, stdout: &mut impl Write) -> io::Result<ExitCode> {
    match command {
        Command::Push => writeln!(stdout, "wrix beads push: unavailable in this build")?,
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod test {
    use super::Command;

    #[test]
    fn beads_command_parser_accepts_push() {
        assert_eq!(Command::parse("push"), Some(Command::Push));
    }
}
