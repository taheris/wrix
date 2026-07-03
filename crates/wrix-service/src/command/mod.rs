use std::{
    io::{self, Write},
    process::ExitCode,
};

use crate::lifecycle::{self, CacheMode, DoltTransport};

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
    Wait,
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
            "wait" => Some(Self::Wait),
            _ => None,
        }
    }
}

pub const HELP: &str = "Manage workspace services.\n\nUsage: wrix service <command>\n\nCommands:\n  start\n  stop\n  status\n  logs\n  endpoints\n  dolt <status|socket|port|host|attach|gc|wait>\n  cache <status|publish|warm|prune|rotate-key>\n";
pub const DOLT_HELP: &str = "Manage the workspace Dolt service.\n\nUsage: wrix service dolt <command>\n\nCommands:\n  status\n  socket\n  port\n  host\n  attach\n  gc\n  wait\n";

pub fn write_help(stdout: &mut impl Write) -> io::Result<()> {
    stdout.write_all(HELP.as_bytes())
}

pub fn write_dolt_help(stdout: &mut impl Write) -> io::Result<()> {
    stdout.write_all(DOLT_HELP.as_bytes())
}

pub fn run_top(command: Top, args: &[String], stdout: &mut impl Write) -> io::Result<ExitCode> {
    let cache_mode = parse_cache_mode(args)?;
    match command {
        Top::Start => {
            let status = lifecycle::start(cache_mode).map_err(io::Error::other)?;
            stdout.write_all(status.render().as_bytes())?;
        }
        Top::Stop => {
            let status = lifecycle::stop(cache_mode).map_err(io::Error::other)?;
            stdout.write_all(status.render().as_bytes())?;
        }
        Top::Status => {
            let status = lifecycle::status(cache_mode).map_err(io::Error::other)?;
            stdout.write_all(status.render().as_bytes())?;
        }
        Top::Logs => {
            let logs = lifecycle::logs(cache_mode).map_err(io::Error::other)?;
            stdout.write_all(&logs)?;
        }
        Top::Endpoints => {
            let endpoints = lifecycle::endpoints(cache_mode).map_err(io::Error::other)?;
            stdout.write_all(endpoints.as_bytes())?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub fn run_dolt(command: Dolt, stdout: &mut impl Write) -> io::Result<ExitCode> {
    let plan = lifecycle::Plan::for_current_dir(CacheMode::Disabled).map_err(io::Error::other)?;
    let Some(endpoint) = plan.dolt() else {
        writeln!(stdout, "dolt: disabled")?;
        return Ok(ExitCode::FAILURE);
    };

    match command {
        Dolt::Status => {
            writeln!(stdout, "dolt: enabled")?;
            writeln!(stdout, "transport: {}", endpoint.transport().as_str())?;
            writeln!(stdout, "socket: {}", endpoint.socket_path().display())?;
            if let Some(port) = endpoint.tcp_port() {
                writeln!(stdout, "host: 127.0.0.1")?;
                writeln!(stdout, "port: {port}")?;
            }
        }
        Dolt::Socket => {
            if endpoint.transport() == DoltTransport::UnixSocket {
                writeln!(stdout, "{}", endpoint.socket_path().display())?;
            } else {
                writeln!(stdout, "dolt socket unavailable for tcp transport")?;
                return Ok(ExitCode::FAILURE);
            }
        }
        Dolt::Port => {
            if let Some(port) = endpoint.tcp_port() {
                writeln!(stdout, "{port}")?;
            } else {
                writeln!(stdout, "dolt tcp port unavailable for unix transport")?;
                return Ok(ExitCode::FAILURE);
            }
        }
        Dolt::Host => {
            if let Some(host) = endpoint.tcp_host() {
                writeln!(stdout, "{host}")?;
            } else {
                writeln!(stdout, "dolt tcp host unavailable for unix transport")?;
                return Ok(ExitCode::FAILURE);
            }
        }
        Dolt::Attach => {
            writeln!(stdout, "{}", attach_command(endpoint))?;
        }
        Dolt::Gc => {
            writeln!(
                stdout,
                "dolt gc: run against {}",
                plan.workspace()
                    .canonical_path()
                    .join(".beads/dolt")
                    .display()
            )?;
        }
        Dolt::Wait => {
            lifecycle::wait_for_dolt(CacheMode::Disabled).map_err(io::Error::other)?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn attach_command(endpoint: &lifecycle::DoltEndpoint) -> String {
    match endpoint.transport() {
        DoltTransport::UnixSocket => format!(
            "dolt sql-client --socket {}",
            endpoint.socket_path().display()
        ),
        DoltTransport::Tcp => format!(
            "dolt sql-client --host 127.0.0.1 --port {}",
            endpoint
                .tcp_port()
                .map_or_else(|| String::from("<unavailable>"), |port| port.to_string())
        ),
    }
}

fn parse_cache_mode(args: &[String]) -> io::Result<CacheMode> {
    let mut cache_mode = CacheMode::Enabled;
    for arg in args {
        match arg.as_str() {
            "--no-cache" => cache_mode = CacheMode::Disabled,
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown service option: {other}"),
                ));
            }
        }
    }
    Ok(cache_mode)
}

#[cfg(test)]
mod test {
    use super::{Dolt, Top, parse_cache_mode};
    use crate::lifecycle::CacheMode;

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
        assert_eq!(Dolt::parse("wait"), Some(Dolt::Wait));
    }

    #[test]
    fn no_cache_option_disables_cache_startup() {
        let args = vec![String::from("--no-cache")];
        assert_eq!(parse_cache_mode(&args).unwrap(), CacheMode::Disabled);
    }
}
