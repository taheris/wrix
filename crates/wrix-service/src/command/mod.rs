use std::{
    io::{self, Write},
    process::ExitCode,
};

use crate::lifecycle::{self, CacheMode, DoltTransport};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Top {
    Capabilities,
    Start,
    Recover,
    Repair,
    Rename,
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
    SandboxEndpoint,
    Client,
    SandboxClient,
    ClientEnv,
    SyncPush,
    SyncPull,
    Attach,
    Gc,
    Wait,
}

impl Top {
    pub fn parse(input: &str) -> Option<Self> {
        match input {
            "capabilities" => Some(Self::Capabilities),
            "start" => Some(Self::Start),
            "recover" => Some(Self::Recover),
            "repair" => Some(Self::Repair),
            "rename" => Some(Self::Rename),
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
            "sandbox-endpoint" => Some(Self::SandboxEndpoint),
            "client" => Some(Self::Client),
            "sandbox-client" => Some(Self::SandboxClient),
            "client-env" => Some(Self::ClientEnv),
            "sync-push" => Some(Self::SyncPush),
            "sync-pull" => Some(Self::SyncPull),
            "attach" => Some(Self::Attach),
            "gc" => Some(Self::Gc),
            "wait" => Some(Self::Wait),
            _ => None,
        }
    }
}

pub const CAPABILITIES: &str = "shared-services-v1\n";

pub const HELP: &str = "Manage workspace services.\n\nUsage: wrix service <command> [options]\n\nCommands:\n  capabilities  Print the passive shared-service protocol marker.\n  start      Start the workspace services.\n  recover    Recover the absent or stopped selected cache owner without deleting data.\n  repair     Explicitly upgrade the recorded host controller, then recover services.\n  rename <name>  Rename an inactive selected owner without resetting its data.\n  stop       Stop the workspace services.\n  status     Show workspace service status.\n  logs       Print workspace service logs.\n  endpoints  Print workspace service endpoints.\n  dolt <status|socket|port|host|sandbox-endpoint|attach|gc|wait>       Manage the workspace Dolt service.\n  cache <config|status|publish|warm|rotate-key>        Manage the workspace project cache.\n  nix-store [enable|disable|gc]  Inspect approval or request scoped installed-store collection.\n\nOptions:\n  --no-cache  Disable project-cache handling for top-level service commands.\n  --sandbox-cache  Resolve the project-cache endpoint for sandbox reads (endpoints only).\n  -h, --help  Print help.\n";
pub const DOLT_HELP: &str = "Manage the workspace Dolt service.\n\nUsage: wrix service dolt <command>\n\nCommands:\n  status  Show Dolt service status and connection details.\n  socket  Print the Dolt Unix-socket path.\n  port    Print the Dolt TCP port.\n  host    Print the Dolt TCP host.\n  sandbox-endpoint  Print the sandbox-visible Dolt TCP endpoint.\n  attach  Print a command that connects to Dolt.\n  gc      Print the Dolt garbage-collection target.\n  wait    Wait until the Dolt service is ready.\n\nOptions:\n  -h, --help  Print help.\n";

pub fn write_help(stdout: &mut impl Write) -> io::Result<()> {
    stdout.write_all(HELP.as_bytes())
}

pub fn write_dolt_help(stdout: &mut impl Write) -> io::Result<()> {
    stdout.write_all(DOLT_HELP.as_bytes())
}

pub fn run_top(command: Top, args: &[String], stdout: &mut impl Write) -> io::Result<ExitCode> {
    let options = if command == Top::Rename {
        &[][..]
    } else {
        args
    };
    let (cache_mode, endpoint_view) = parse_options(command, options)?;
    match command {
        Top::Capabilities => stdout.write_all(CAPABILITIES.as_bytes())?,
        Top::Start => {
            let status = lifecycle::start(cache_mode).map_err(io::Error::other)?;
            stdout.write_all(status.render().as_bytes())?;
        }
        Top::Recover => {
            let report = lifecycle::recover().map_err(io::Error::other)?;
            stdout.write_all(report.as_bytes())?;
        }
        Top::Repair => {
            let report = lifecycle::repair().map_err(io::Error::other)?;
            stdout.write_all(report.as_bytes())?;
        }
        Top::Rename => {
            let [name] = args else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "expected: wrix service rename <name>",
                ));
            };
            let name = wrix_core::path::ContainerName::from_persisted(name.clone())
                .filter(|name| {
                    name.as_str().len() <= 128
                        && name
                            .as_str()
                            .starts_with(|ch: char| ch.is_ascii_alphanumeric())
                })
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid service container name",
                    )
                })?;
            let report = lifecycle::rename(&name).map_err(io::Error::other)?;
            stdout.write_all(report.as_bytes())?;
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
            let endpoints = match endpoint_view {
                EndpointView::Host => lifecycle::endpoints(cache_mode),
                EndpointView::SandboxCache => lifecycle::sandbox_cache_endpoints(cache_mode),
            }
            .map_err(io::Error::other)?;
            stdout.write_all(endpoints.as_bytes())?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub fn run_dolt(command: Dolt, stdout: &mut impl Write) -> io::Result<ExitCode> {
    match command {
        Dolt::Client | Dolt::SandboxClient | Dolt::ClientEnv => {
            let client = lifecycle::shared::client(command == Dolt::SandboxClient)
                .map_err(io::Error::other)?;
            if command == Dolt::ClientEnv {
                let plan = lifecycle::Plan::for_current_dir(CacheMode::Disabled)
                    .map_err(io::Error::other)?;
                writeln!(
                    stdout,
                    "export BEADS_DOLT_AUTO_START=0 BEADS_DOLT_SERVER_MODE=1 BEADS_DOLT_SHARED_SERVER=1"
                )?;
                match plan
                    .dolt()
                    .ok_or_else(|| io::Error::other("missing managed endpoint"))?
                {
                    lifecycle::DoltEndpoint::UnixSocket { socket_path } => {
                        writeln!(
                            stdout,
                            "unset BEADS_DOLT_SERVER_HOST BEADS_DOLT_SERVER_PORT"
                        )?;
                        writeln!(
                            stdout,
                            "export BEADS_DOLT_SERVER_SOCKET={}",
                            shell_quote(&socket_path.as_path().to_string_lossy())
                        )?;
                    }
                    lifecycle::DoltEndpoint::Tcp { port, .. } => {
                        writeln!(stdout, "unset BEADS_DOLT_SERVER_SOCKET")?;
                        writeln!(
                            stdout,
                            "export BEADS_DOLT_SERVER_HOST=127.0.0.1 BEADS_DOLT_SERVER_PORT={port}"
                        )?;
                    }
                }
                let sync = serde_json::to_string(&client.sync).map_err(io::Error::other)?;
                for (name, value) in [
                    ("BEADS_DOLT_SERVER_USER", client.credentials.user()),
                    ("BEADS_DOLT_PASSWORD", client.credentials.password()),
                    ("BEADS_DOLT_SERVER_DATABASE", client.credentials.database()),
                    ("WRIX_BEADS_SYNC_ENDPOINT", sync.as_str()),
                    ("WRIX_BEADS_MANAGED", "1"),
                    (
                        "WRIX_BEADS_WORKTREE",
                        client
                            .worktree
                            .to_str()
                            .ok_or_else(|| io::Error::other("non-Unicode managed worktree"))?,
                    ),
                ] {
                    writeln!(stdout, "export {name}={}", shell_quote(value))?;
                }
            } else {
                serde_json::to_writer(&mut *stdout, &client).map_err(io::Error::other)?;
                writeln!(stdout)?;
            }
            return Ok(ExitCode::SUCCESS);
        }
        Dolt::SyncPush | Dolt::SyncPull => {
            let operation = if command == Dolt::SyncPush {
                crate::dolt::Sync::Push
            } else {
                crate::dolt::Sync::Pull
            };
            let reply = lifecycle::shared::sync(operation).map_err(io::Error::other)?;
            match reply {
                crate::dolt::broker::Reply::Applied => return Ok(ExitCode::SUCCESS),
                crate::dolt::broker::Reply::NonFastForward => {
                    writeln!(stdout, "non-fast-forward: namespace remote advanced")?;
                }
                crate::dolt::broker::Reply::Rejected => {
                    writeln!(stdout, "namespace sync authorization rejected")?;
                }
                crate::dolt::broker::Reply::Failed => writeln!(
                    stdout,
                    "namespace sync failed; no automatic pull was attempted"
                )?,
            }
            return Ok(ExitCode::FAILURE);
        }
        _ => (),
    }
    let plan = lifecycle::Plan::for_current_dir(CacheMode::Disabled).map_err(io::Error::other)?;
    let Some(endpoint) = plan.dolt() else {
        writeln!(stdout, "dolt: disabled")?;
        return Ok(ExitCode::FAILURE);
    };

    match command {
        Dolt::Client | Dolt::SandboxClient | Dolt::ClientEnv | Dolt::SyncPush | Dolt::SyncPull => {
            return Err(io::Error::other("invalid client dispatch"));
        }
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
        Dolt::SandboxEndpoint => {
            writeln!(
                stdout,
                "{}",
                lifecycle::sandbox_dolt_endpoint(CacheMode::Disabled).map_err(io::Error::other)?
            )?;
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

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn attach_command(endpoint: &lifecycle::DoltEndpoint) -> String {
    match endpoint {
        lifecycle::DoltEndpoint::UnixSocket { socket_path } => {
            format!(
                "dolt sql-client --socket {}",
                socket_path.as_path().display()
            )
        }
        lifecycle::DoltEndpoint::Tcp { port, .. } => {
            format!("dolt sql-client --host 127.0.0.1 --port {port}")
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EndpointView {
    Host,
    SandboxCache,
}

fn parse_options(command: Top, args: &[String]) -> io::Result<(CacheMode, EndpointView)> {
    let mut cache_mode = CacheMode::Enabled;
    let mut endpoint_view = EndpointView::Host;
    for arg in args {
        match arg.as_str() {
            "--no-cache"
                if !matches!(
                    command,
                    Top::Recover | Top::Repair | Top::Rename | Top::Capabilities
                ) =>
            {
                cache_mode = CacheMode::Disabled;
            }
            "--sandbox-cache" if command == Top::Endpoints => {
                endpoint_view = EndpointView::SandboxCache;
            }
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown service option: {other}"),
                ));
            }
        }
    }
    Ok((cache_mode, endpoint_view))
}

#[cfg(test)]
mod test {
    use super::{Dolt, EndpointView, Top, parse_options};
    use crate::lifecycle::CacheMode;

    #[test]
    fn top_command_parser_accepts_public_service_commands() {
        assert_eq!(Top::parse("capabilities"), Some(Top::Capabilities));
        assert_eq!(Top::parse("start"), Some(Top::Start));
        assert_eq!(Top::parse("recover"), Some(Top::Recover));
        assert_eq!(Top::parse("repair"), Some(Top::Repair));
        assert_eq!(Top::parse("rename"), Some(Top::Rename));
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
    fn recovery_rejects_cache_opt_out_and_scope_overrides() {
        for command in [Top::Recover, Top::Repair, Top::Rename] {
            for option in ["--no-cache", "--host", "--sandbox-cache", "unexpected"] {
                assert!(parse_options(command, &[option.into()]).is_err());
            }
        }
    }

    #[test]
    fn no_cache_option_disables_cache_startup() {
        let args = vec![String::from("--no-cache")];
        assert_eq!(
            parse_options(Top::Start, &args).unwrap(),
            (CacheMode::Disabled, EndpointView::Host)
        );
    }

    #[test]
    fn sandbox_cache_option_is_scoped_to_endpoint_queries() {
        let args = [String::from("--sandbox-cache")];
        assert_eq!(
            parse_options(Top::Endpoints, &args).unwrap(),
            (CacheMode::Enabled, EndpointView::SandboxCache)
        );
        for command in [Top::Start, Top::Stop, Top::Status, Top::Logs] {
            assert!(parse_options(command, &args).is_err());
        }
    }
}
