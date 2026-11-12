use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// unsupported external runtime invocation
    Usage,
    /// shared runtime fixture I/O failed: {0}
    Io(#[from] io::Error),
    /// shared runtime fixture JSON failed: {0}
    Json(#[from] serde_json::Error),
    /// missing shared runtime fixture environment: {0}
    Environment(&'static str),
    /// no such container
    Missing,
    /// owned native subprocess failed: {0}
    Child(std::process::ExitStatus),
    /// fixture rejects removal of a running backend
    Running,
    /// native service did not finish its graceful stop
    Timeout,
}

#[derive(Debug, Deserialize, Serialize)]
struct State {
    name: String,
    labels: BTreeMap<String, String>,
    ports: Vec<String>,
    running: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    boot: Option<String>,
}

fn environment(name: &'static str) -> Result<OsString, Error> {
    std::env::var_os(name).ok_or(Error::Environment(name))
}

fn digest() -> String {
    format!("sha256:{}", "a".repeat(64))
}

fn read_optional<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, Error> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn save(record: &Path, state: &State) -> Result<(), Error> {
    fs::write(record, serde_json::to_vec(state)?)?;
    Ok(())
}

fn checked(command: &mut Command) -> Result<(), Error> {
    let status = command.status()?;
    if !status.success() {
        return Err(Error::Child(status));
    }
    Ok(())
}

fn control(args: &[&str]) -> Result<(), Error> {
    checked(
        Command::new(environment("WRIX_TEST_SQL_CLIENT")?)
            .arg(environment("WRIX_TEST_ROLES_CONTROL")?)
            .args(args),
    )
}

fn compiler_build_context(command: &mut Command) {
    // The private service namespace builds here, not in the outer derivation's
    // build directory. Keep the linker's purity guard enabled and scoped to the
    // actual compiler execution tree rather than disabling it for /run outputs.
    command.env("NIX_BUILD_TOP", "/run/wrix/compiler/execution");
}

fn launch(root: &Path, data: &Path, transport: &str, boot: Option<&str>) -> Result<u32, Error> {
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("service.log"))?;
    let mut command = Command::new("setsid");
    command.args([
        "unshare", "--mount", "--propagation", "private", "bash", "-c",
        "set -euo pipefail; mount -t tmpfs -o mode=755 tmpfs /run; mkdir -m 700 /run/wrix; exec \"$@\"", "fixture-backend",
    ]).arg(environment("WRIX_SHARED_BINARY")?).arg(data).arg(transport)
        .env("SCCACHE_LOG", "error")
        .env("SCCACHE_ERROR_LOG", root.join("compiler.log"))
        .stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log);
    compiler_build_context(&mut command);
    if let Some(boot) = boot {
        command.env("WRIX_BACKEND_BOOT", boot);
    }
    Ok(command.spawn()?.id())
}

fn options(args: &[String], key: &str) -> Result<Vec<String>, Error> {
    args.iter()
        .enumerate()
        .filter(|(_, word)| word.as_str() == key)
        .map(|(index, _)| args.get(index + 1).cloned().ok_or(Error::Usage))
        .collect()
}

fn option(args: &[String], key: &str) -> Result<String, Error> {
    options(args, key)?.into_iter().next().ok_or(Error::Usage)
}

fn inspect(state: Option<&State>, args: &[String]) -> Result<String, Error> {
    let state = state.ok_or(Error::Missing)?;
    if args.len() != 5 || args[3] != state.name || args[4] != "--type=container" {
        return Err(Error::Missing);
    }
    match args[2].as_str() {
        "{{.State.Running}}" => Ok(state.running.to_string()),
        "{{.Image}}" => Ok(digest()),
        template => {
            let label = template.split('"').nth(1).ok_or(Error::Usage)?;
            Ok(state.labels.get(label).cloned().unwrap_or_default())
        }
    }
}

fn stop(state: &State) -> Result<(), Error> {
    let pid = state.pid.ok_or(Error::Usage)?;
    checked(Command::new("kill").args(["-TERM", "--", &format!("-{pid}")]))?;
    let owner = OpenOptions::new()
        .read(true)
        .write(true)
        .open(state.data.as_ref().ok_or(Error::Usage)?.join("owner.lock"))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match owner.try_lock_exclusive() {
            Ok(()) => {
                FileExt::unlock(&owner)?;
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(Error::Timeout);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn execute(root: &Path, state: Option<&State>, args: &[String]) -> Result<ExitCode, Error> {
    let mut index = 1;
    let mut interactive = false;
    let mut environment = BTreeMap::new();
    while let Some(argument) = args.get(index) {
        match argument.as_str() {
            "-i" => {
                interactive = true;
                index += 1;
            }
            "-e" => {
                let (key, value) = args
                    .get(index + 1)
                    .and_then(|value| value.split_once('='))
                    .ok_or(Error::Usage)?;
                environment.insert(key, value);
                index += 2;
            }
            _ => break,
        }
    }
    let state = state.ok_or(Error::Missing)?;
    if !state.running || args.get(index) != Some(&state.name) {
        return Err(Error::Missing);
    }
    let command = args.get(index + 1..).ok_or(Error::Usage)?;
    let (program, arguments) = command.split_first().ok_or(Error::Usage)?;
    let program = if program == "wrix-shared-service" {
        environment_value()?
    } else {
        OsString::from(program)
    };
    let output = Command::new("nsenter")
        .args([
            "--mount",
            "--target",
            &state.pid.ok_or(Error::Usage)?.to_string(),
            "--",
        ])
        .arg(program)
        .args(arguments)
        .envs(environment)
        .current_dir(state.data.as_ref().ok_or(Error::Usage)?)
        .stdin(if interactive {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .output()?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("native-exec.stderr"))?
        .write_all(&output.stderr)?;
    io::stdout().lock().write_all(&output.stdout)?;
    io::stderr().lock().write_all(&output.stderr)?;
    let code = output
        .status
        .code()
        .or_else(|| {
            use std::os::unix::process::ExitStatusExt;
            output.status.signal().map(|signal| -signal)
        })
        .ok_or(Error::Usage)?;
    Ok(ExitCode::from(
        u8::try_from(code.rem_euclid(256)).map_err(|_| Error::Usage)?,
    ))
}

fn environment_value() -> Result<OsString, Error> {
    environment("WRIX_SHARED_BINARY")
}

fn run_container(root: &Path, record: &Path, args: &[String]) -> Result<(), Error> {
    let labels = options(args, "--label")?
        .into_iter()
        .map(|value| {
            let (key, value) = value.split_once('=').ok_or(Error::Usage)?;
            Ok((key.to_owned(), value.to_owned()))
        })
        .collect::<Result<BTreeMap<_, _>, Error>>()?;
    let ports = options(args, "-p")?;
    let index = args
        .iter()
        .position(|word| word == "wrix-shared-service")
        .ok_or(Error::Usage)?;
    let command = &args[index + 1..];
    let name = option(args, "--name")?;
    if command == ["gateway"] {
        let mut control_args = vec!["gateway"];
        control_args.extend(args.iter().map(String::as_str));
        control(&control_args)?;
        return save(
            &root.join("gateway.json"),
            &State {
                name,
                labels,
                ports,
                running: true,
                pid: None,
                data: None,
                transport: None,
                boot: None,
            },
        );
    }
    let [data, transport] = command else {
        return Err(Error::Usage);
    };
    fs::write(root.join("run.argv"), args.join("\n"))?;
    for published in &ports {
        let (listen, target) = published.rsplit_once(':').ok_or(Error::Usage)?;
        if matches!(target, "3306" | "3307" | "4226") {
            control(&["forward", listen, &format!("127.0.0.1:{target}")])?;
        }
    }
    let boot = environment("WRIX_BACKEND_BOOT")?
        .into_string()
        .map_err(|_| Error::Usage)?;
    let pid = launch(root, Path::new(data), transport, None)?;
    save(
        record,
        &State {
            name,
            labels,
            ports,
            running: true,
            pid: Some(pid),
            data: Some(data.into()),
            transport: Some(transport.clone()),
            boot: Some(boot),
        },
    )
}

pub(super) fn run(args: &[OsString]) -> Result<ExitCode, Error> {
    let Some((engine, args)) = args.split_first() else {
        return Err(Error::Usage);
    };
    let args = args
        .iter()
        .map(|arg| arg.to_str().map(String::from).ok_or(Error::Usage))
        .collect::<Result<Vec<_>, _>>()?;
    if args == ["--version"] {
        writeln!(io::stdout().lock(), "podman version 6.1.4")?;
        return Ok(ExitCode::SUCCESS);
    }
    if args == ["info", "--format", "json"] {
        writeln!(
            io::stdout().lock(),
            "{}",
            json!({"version":{"Version":"6.1.4"},"host":{"security":{"rootless":true}},"store":{"graphRoot":"/home/test/containers/storage","runRoot":"/run/user/1000/containers"}})
        )?;
        return Ok(ExitCode::SUCCESS);
    }
    let root = PathBuf::from(environment("WRIX_SHARED_FIXTURE")?);
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("runtime.log"))?;
    serde_json::to_writer(&mut log, &args)?;
    log.write_all(b"\n")?;
    if engine == "container" {
        let legacy: Vec<Value> =
            read_optional(&root.join("retained-legacy.json"))?.unwrap_or_default();
        if args == ["list", "--all", "--format", "json"]
            || (args.first().is_some_and(|arg| arg == "inspect")
                && legacy
                    .first()
                    .and_then(|value| value["configuration"]["id"].as_str())
                    == args.last().map(String::as_str))
        {
            writeln!(io::stdout().lock(), "{}", serde_json::to_string(&legacy)?)?;
            return Ok(ExitCode::SUCCESS);
        }
        return Ok(ExitCode::from(64));
    }
    if engine != "podman" {
        return Err(Error::Usage);
    }
    let gateway: Option<State> = read_optional(&root.join("gateway.json"))?;
    let selected = if args.last().is_some_and(|arg| arg == "--type=container") {
        args.get(args.len().saturating_sub(2))
    } else {
        args.last()
    };
    let record = root.join(
        if gateway
            .as_ref()
            .is_some_and(|state| selected == Some(&state.name))
        {
            "gateway.json"
        } else {
            "runtime.json"
        },
    );
    let mut state: Option<State> = read_optional(&record)?;
    let words: Vec<_> = args.iter().map(String::as_str).collect();
    let output = match words.as_slice() {
        ["image", "inspect", ..] => Some(digest()),
        ["image", "exists" | "tag", ..] | ["tag", ..] => None,
        ["ps", "-a", "--format", "{{.Names}}"] => state.as_ref().map(|state| state.name.clone()),
        ["inspect", ..] => Some(inspect(state.as_ref(), &args)?),
        ["port", ..] => {
            let ports = state
                .as_ref()
                .map(|state| state.ports.as_slice())
                .unwrap_or_default();
            let rendered = ports
                .iter()
                .map(|port| {
                    let (listen, target) = port.rsplit_once(':').ok_or(Error::Usage)?;
                    Ok(format!("{target}/tcp -> {listen}"))
                })
                .collect::<Result<Vec<_>, Error>>()?;
            Some(rendered.join("\n"))
        }
        ["run", ..] => {
            run_container(&root, &record, &args)?;
            None
        }
        ["exec", ..] => return execute(&root, state.as_ref(), &args),
        ["start", ..] => {
            let state = state.as_mut().ok_or(Error::Missing)?;
            state.pid = Some(launch(
                &root,
                state.data.as_ref().ok_or(Error::Usage)?,
                state.transport.as_deref().ok_or(Error::Usage)?,
                state.boot.as_deref(),
            )?);
            state.running = true;
            save(&record, state)?;
            None
        }
        ["stop", ..] => {
            let state = state.as_mut().ok_or(Error::Missing)?;
            stop(state)?;
            state.running = false;
            save(&record, state)?;
            None
        }
        ["rm", ..] => {
            if state.as_ref().is_some_and(|state| state.running) {
                return Err(Error::Running);
            }
            match fs::remove_file(record) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            None
        }
        ["logs", ..] => Some(fs::read_to_string(root.join("service.log"))?),
        _ => return Err(Error::Usage),
    };
    if let Some(output) = output
        && !output.is_empty()
    {
        writeln!(io::stdout().lock(), "{output}")?;
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod exec_test;

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn compiler_build_context_preserves_purity_and_uses_private_execution_tree() {
        let mut command = Command::new("bash");
        command
            .args([
                "-c",
                "set -euo pipefail; [[ $NIX_ENFORCE_PURITY == 1 ]]; printf '%s' \"$NIX_BUILD_TOP\"",
            ])
            .env("NIX_ENFORCE_PURITY", "1")
            .env("NIX_BUILD_TOP", "/outer-derivation");
        compiler_build_context(&mut command);
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"/run/wrix/compiler/execution");
    }

    #[test]
    fn native_inspection_preserves_running_state_image_labels_and_missing_errors() {
        let state: State = serde_json::from_value(
            json!({"name":"owned","running":false,"labels":{"wrix.kind":"service"},"ports":[]}),
        )
        .unwrap();
        let args = |template: &str| {
            ["inspect", "--format", template, "owned", "--type=container"].map(String::from)
        };
        assert_eq!(
            inspect(Some(&state), &args("{{.State.Running}}")).unwrap(),
            "false"
        );
        assert_eq!(
            inspect(Some(&state), &args("{{.Image}}")).unwrap(),
            digest()
        );
        assert_eq!(
            inspect(
                Some(&state),
                &args("{{ index .Config.Labels \"wrix.kind\" }}")
            )
            .unwrap(),
            "service"
        );
        assert_eq!(
            inspect(
                Some(&state),
                &args("{{ index .Config.Labels \"unknown\" }}")
            )
            .unwrap(),
            ""
        );
        assert!(matches!(
            inspect(None, &args("{{.Image}}")),
            Err(Error::Missing)
        ));
    }

    #[test]
    fn runtime_state_roundtrip_preserves_owned_pid_boot_transport_and_ports() {
        let root = tempfile::tempdir().unwrap();
        let record = root.path().join("runtime.json");
        let value = json!({"name":"owned","labels":{},"ports":["127.0.0.1:1234:3306"],"running":true,"pid":42,"data":"/owned","transport":"unix","boot":"epoch"});
        let state: State = serde_json::from_value(value.clone()).unwrap();
        save(&record, &state).unwrap();
        assert_eq!(read_optional::<Value>(&record).unwrap(), Some(value));
        assert!(
            read_optional::<Value>(&root.path().join("absent"))
                .unwrap()
                .is_none()
        );
        fs::write(record.clone(), "not-json").unwrap();
        assert!(read_optional::<State>(&record).is_err());
    }

    #[test]
    fn repeated_runtime_flags_preserve_order_and_reject_missing_values() {
        let args = [
            "run",
            "--label",
            "x=one",
            "--label",
            "y=two=three",
            "-p",
            "127.0.0.1:1234:3306",
        ]
        .map(String::from);
        assert_eq!(options(&args, "--label").unwrap(), ["x=one", "y=two=three"]);
        assert_eq!(option(&args, "-p").unwrap(), "127.0.0.1:1234:3306");
        assert!(option(&args, "--name").is_err());
        assert!(options(&["run", "--label"].map(String::from), "--label").is_err());
    }
}
