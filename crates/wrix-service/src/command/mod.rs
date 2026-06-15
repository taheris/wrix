use std::{
    io::{self, Write},
    process::ExitCode,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Top {
    Start,
    Stop,
    Status,
    Logs,
    Endpoints,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Dolt {
    Status,
    Socket,
    Port,
    Host,
    Attach,
    Gc,
}

impl Top {
    pub fn parse(input: &str) -> Option<Self> {
        match input {
            "start" => Some(Self::Start),
            "stop" => Some(Self::Stop),
            "status" => Some(Self::Status),
            "logs" => Some(Self::Logs),
            "endpoints" => Some(Self::Endpoints),
            _ => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Status => "status",
            Self::Logs => "logs",
            Self::Endpoints => "endpoints",
        }
    }
}

impl Dolt {
    pub fn parse(input: &str) -> Option<Self> {
        match input {
            "status" => Some(Self::Status),
            "socket" => Some(Self::Socket),
            "port" => Some(Self::Port),
            "host" => Some(Self::Host),
            "attach" => Some(Self::Attach),
            "gc" => Some(Self::Gc),
            _ => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Socket => "socket",
            Self::Port => "port",
            Self::Host => "host",
            Self::Attach => "attach",
            Self::Gc => "gc",
        }
    }
}

pub fn write_help(stdout: &mut impl Write) -> io::Result<()> {
    stdout.write_all(
        b"Manage workspace services.\n\nUsage: wrix service <command>\n\nCommands:\n  start\n  stop\n  status\n  logs\n  endpoints\n  dolt <status|socket|port|host|attach|gc>\n  cache <status|publish|warm|prune|rotate-key>\n",
    )
}

pub fn write_dolt_help(stdout: &mut impl Write) -> io::Result<()> {
    stdout.write_all(
        b"Manage the workspace Dolt service.\n\nUsage: wrix service dolt <command>\n\nCommands:\n  status\n  socket\n  port\n  host\n  attach\n  gc\n",
    )
}

pub fn run_top(command: Top, stdout: &mut impl Write) -> io::Result<ExitCode> {
    writeln!(
        stdout,
        "wrix service {}: unavailable in this build",
        command.as_str()
    )?;
    Ok(ExitCode::SUCCESS)
}

pub fn run_dolt(command: Dolt, stdout: &mut impl Write) -> io::Result<ExitCode> {
    writeln!(
        stdout,
        "wrix service dolt {}: unavailable in this build",
        command.as_str()
    )?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod test {
    use super::{Dolt, Top};

    #[test]
    fn top_command_parser_accepts_public_service_commands() {
        assert_eq!(Top::parse("start"), Some(Top::Start));
        assert_eq!(Top::parse("stop"), Some(Top::Stop));
        assert_eq!(Top::parse("status"), Some(Top::Status));
        assert_eq!(Top::parse("logs"), Some(Top::Logs));
        assert_eq!(Top::parse("endpoints"), Some(Top::Endpoints));
    }

    #[test]
    fn dolt_command_parser_accepts_public_dolt_commands() {
        assert_eq!(Dolt::parse("status"), Some(Dolt::Status));
        assert_eq!(Dolt::parse("socket"), Some(Dolt::Socket));
        assert_eq!(Dolt::parse("port"), Some(Dolt::Port));
        assert_eq!(Dolt::parse("host"), Some(Dolt::Host));
        assert_eq!(Dolt::parse("attach"), Some(Dolt::Attach));
        assert_eq!(Dolt::parse("gc"), Some(Dolt::Gc));
    }
}
