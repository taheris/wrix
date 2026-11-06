use serde::Deserialize;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    io::{self, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, ExitCode, Output},
};
#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// unsupported native subprocess fixture arguments
    Usage,
    /// native fixture environment is missing or invalid: {0}
    Environment(&'static str),
    /// native subprocess fixture I/O failed: {0}
    Io(#[from] io::Error),
    /// native subprocess fixture JSON failed: {0}
    Json(#[from] serde_json::Error),
    /// native fixture configuration has no parent
    Parent,
    /// owned fixture runtime executable is missing
    Runtime,
    /// native fixture process exit status could not be represented
    ExitStatus,
}

fn environment(name: &'static str) -> Result<OsString, Error> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .ok_or(Error::Environment(name))
}

fn exit_status(status: std::process::ExitStatus) -> Result<ExitCode, Error> {
    use std::os::unix::process::ExitStatusExt;
    let code = status
        .code()
        .or_else(|| status.signal().map(|signal| -signal))
        .ok_or(Error::ExitStatus)?;
    let code = u8::try_from(code.rem_euclid(256)).map_err(|_| Error::ExitStatus)?;
    Ok(ExitCode::from(code))
}

#[derive(Debug, Deserialize)]
struct Reply {
    stdout: String,
    stderr: String,
    success: bool,
}

#[derive(Debug)]
enum CliResult {
    Reply(Reply),
    HarnessFailure(Output),
}

fn shared_cli_result(
    root: &Path,
    executable: &OsStr,
    args: &[OsString],
) -> Result<CliResult, Error> {
    let arguments = args
        .iter()
        .map(|arg| arg.to_str().ok_or(Error::Usage))
        .collect::<Result<Vec<_>, _>>()?;
    let temporary = tempfile::Builder::new()
        .prefix("wrix-cli-")
        .tempdir_in(root)?;
    let result = temporary.path().join("result.json");
    let output = Command::new(executable)
        .args(["--exact", "subprocess_entry", "--nocapture"])
        .env("WRIX_SHARED_FIXTURE", root)
        .env("WRIX_SHARED_ARGUMENTS", serde_json::to_string(&arguments)?)
        .env("WRIX_SHARED_RESULT", &result)
        .output()?;
    if !output.status.success() {
        return Ok(CliResult::HarnessFailure(output));
    }
    Ok(CliResult::Reply(serde_json::from_slice(&fs::read(
        result,
    )?)?))
}

pub(super) fn shared_cli(args: &[OsString]) -> Result<ExitCode, Error> {
    let root = PathBuf::from(environment("WRIX_SHARED_FIXTURE")?);
    let executable = environment("WRIX_SHARED_TEST_EXE")?;
    match shared_cli_result(&root, &executable, args)? {
        CliResult::HarnessFailure(output) => {
            io::stderr().lock().write_all(&output.stderr)?;
            exit_status(output.status)
        }
        CliResult::Reply(reply) => {
            io::stdout().lock().write_all(reply.stdout.as_bytes())?;
            io::stderr().lock().write_all(reply.stderr.as_bytes())?;
            Ok(if reply.success {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
    }
}

fn backend_command(
    configuration: &Path,
    engine: &str,
    args: &[OsString],
) -> Result<Command, Error> {
    if !matches!(engine, "container" | "podman") {
        return Err(Error::Usage);
    }
    let values: BTreeMap<String, String> = serde_json::from_slice(&fs::read(configuration)?)?;
    let program = configuration
        .parent()
        .ok_or(Error::Parent)?
        .join("bin")
        .join(engine);
    if !program.is_file() {
        return Err(Error::Runtime);
    }
    let mut command = Command::new(program);
    command.args(args).envs(values);
    Ok(command)
}

pub(super) fn backend(args: &[OsString]) -> Result<ExitCode, Error> {
    let Some((engine, args)) = args.split_first() else {
        return Err(Error::Usage);
    };
    let engine = engine.to_str().ok_or(Error::Usage)?;
    let configuration = environment("WRIX_TEST_BACKEND_ENV")?;
    Err(backend_command(Path::new(&configuration), engine, args)?
        .exec()
        .into())
}

#[cfg(test)]
mod test {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn dispatcher(root: &Path, body: &str) -> PathBuf {
        let executable = root.join("dispatcher");
        fs::write(
            &executable,
            format!("#!/usr/bin/env bash\nset -euo pipefail\n{body}\n"),
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        executable
    }

    #[test]
    fn shared_cli_preserves_dispatcher_arguments_and_reported_streams() {
        let root = tempfile::tempdir().unwrap();
        let executable = dispatcher(
            root.path(),
            r#"
            test "$#" = 3
            test "$1" = --exact
            test "$2" = subprocess_entry
            test "$3" = --nocapture
            printf '%s' "$WRIX_SHARED_ARGUMENTS" > "$WRIX_SHARED_FIXTURE/arguments.json"
            printf '%s' '{"stdout":"out λ","stderr":"diagnostic\n","success":false}' > "$WRIX_SHARED_RESULT"
        "#,
        );
        let args = [
            OsString::from("two words"),
            OsString::new(),
            OsString::from("λ"),
        ];
        let result = shared_cli_result(root.path(), executable.as_os_str(), &args).unwrap();
        let CliResult::Reply(reply) = result else {
            panic!("dispatcher failed");
        };
        assert!(!reply.success);
        assert_eq!(reply.stdout, "out λ");
        assert_eq!(reply.stderr, "diagnostic\n");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &fs::read(root.path().join("arguments.json")).unwrap()
            )
            .unwrap(),
            serde_json::json!(["two words", "", "λ"])
        );
        assert!(fs::read_dir(root.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("wrix-cli-")
        }));
    }

    #[test]
    fn shared_cli_forwards_harness_failure_without_reading_a_missing_reply() {
        let root = tempfile::tempdir().unwrap();
        let executable = dispatcher(root.path(), "printf harness-failed >&2; exit 37");
        let result = shared_cli_result(root.path(), executable.as_os_str(), &[]).unwrap();
        let CliResult::HarnessFailure(output) = result else {
            panic!("harness failure concealed");
        };
        assert_eq!(output.status.code(), Some(37));
        assert_eq!(output.stderr, b"harness-failed");
    }

    #[test]
    fn shared_cli_rejects_malformed_dispatcher_reports() {
        let root = tempfile::tempdir().unwrap();
        let executable = dispatcher(root.path(), "printf not-json > \"$WRIX_SHARED_RESULT\"");
        assert!(matches!(
            shared_cli_result(root.path(), executable.as_os_str(), &[]),
            Err(Error::Json(_))
        ));
    }

    #[test]
    fn backend_adapter_preserves_args_stdio_environment_and_exit_status() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("bin")).unwrap();
        let program = root.path().join("bin/podman");
        fs::write(&program, "#!/usr/bin/env bash\nIFS= read -r line\nprintf '%s|%s|%s' \"$1\" \"$WRIX_CAPTURED\" \"$line\"\nprintf diagnostic >&2\nexit 37\n").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        let configuration = root.path().join("environment.json");
        fs::write(&configuration, br#"{"WRIX_CAPTURED":"captured value"}"#).unwrap();
        let mut command =
            backend_command(&configuration, "podman", &[OsString::from("two words")]).unwrap();
        let mut child = command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"input\n").unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(37));
        assert_eq!(output.stdout, b"two words|captured value|input");
        assert_eq!(output.stderr, b"diagnostic");
    }

    #[test]
    fn backend_adapter_rejects_unknown_engine_or_missing_owned_program() {
        let root = tempfile::tempdir().unwrap();
        let configuration = root.path().join("environment.json");
        fs::write(&configuration, b"{}").unwrap();
        assert!(matches!(
            backend_command(&configuration, "unexpected", &[]),
            Err(Error::Usage)
        ));
        assert!(matches!(
            backend_command(&configuration, "podman", &[]),
            Err(Error::Runtime)
        ));
    }
}
