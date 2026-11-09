//! External niri IPC fixture for packaged notification acceptance.
use std::{
    env, fs,
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
    thread,
    time::Duration,
};

use serde_json::{Value, json};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
enum Error {
    /// missing fixture environment: {0}
    Environment(&'static str),
    /// desktop fixture I/O: {0}
    Io(#[from] io::Error),
    /// desktop fixture JSON: {0}
    Json(#[from] serde_json::Error),
    /// desktop fixture window list is not an array
    Windows,
}

fn path(name: &'static str) -> Result<PathBuf, Error> {
    env::var_os(name)
        .map(PathBuf::from)
        .ok_or(Error::Environment(name))
}

fn run(args: &[String]) -> Result<ExitCode, Error> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["msg", "-j", "windows"] => {
            io::stdout()
                .lock()
                .write_all(&fs::read(path("WINDOWS")?)?)?;
            io::stdout().lock().write_all(b"\n")?;
        }
        ["msg", "-j", "focused-window"] => {
            writeln!(io::stdout().lock(), "{}", json!({"id":99}))?;
        }
        ["msg", "action", "focus-window", "--id", id] => {
            if env::var("FAIL_FOCUS_WINDOW").as_deref() == Ok(*id) {
                return Ok(ExitCode::from(72));
            }
            if env::var("HOLD_FOCUS_WINDOW").as_deref() == Ok(*id) {
                fs::write(path("FOCUS_READY")?, [])?;
                let release = path("FOCUS_RELEASE")?;
                while !release.try_exists()? {
                    thread::sleep(Duration::from_millis(10));
                }
            }
            let windows = path("WINDOWS")?;
            let mut value: Value = serde_json::from_slice(&fs::read(&windows)?)?;
            for window in value.as_array_mut().ok_or(Error::Windows)? {
                let window_id = window["id"].to_string();
                window["is_focused"] = json!(window_id == *id);
            }
            fs::write(windows, serde_json::to_vec(&value)?)?;
            writeln!(
                fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path("LOG")?)?,
                "{id}"
            )?;
        }
        _ => return Ok(ExitCode::from(64)),
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> Result<ExitCode, Error> {
    run(&env::args().skip(1).collect::<Vec<_>>())
}
