use std::{ffi::OsString, path::PathBuf};

use clap::{Arg, ArgAction, ArgGroup, Command};
use displaydoc::Display;
use thiserror::Error;

use crate::{
    attention,
    navigation::Navigator,
    process::Runner,
    route::{Platform, Target},
};

pub enum Mode {
    Focus,
    Enqueue(Target),
    IsFocused(Target),
    Return,
}

pub fn parser() -> Command {
    Command::new("wrix-focus-notification")
        .about("Visit pending attention, or return to the position before the last jump.")
        .arg(
            Arg::new("enqueue")
                .long("enqueue")
                .value_name("TARGET")
                .help("Queue a registered attention target."),
        )
        .arg(
            Arg::new("is-focused")
                .long("is-focused")
                .value_name("TARGET")
                .help("Check whether a registered target is focused."),
        )
        .arg(
            Arg::new("return")
                .long("return")
                .action(ArgAction::SetTrue)
                .help("Restore the position before the last successful jump (Linux/niri)."),
        )
        .group(
            ArgGroup::new("mode")
                .args(["enqueue", "is-focused", "return"])
                .multiple(false),
        )
}

#[derive(Debug, Display, Error)]
pub enum Error {
    /// invalid target: {0}
    Target(#[from] crate::route::Error),
    /// attention failed: {0}
    Attention(#[from] attention::Error),
    /// HOME is unavailable and no session root was specified
    MissingHome,
}

pub fn mode(matches: &clap::ArgMatches) -> Result<Mode, Error> {
    if matches.get_flag("return") {
        return Ok(Mode::Return);
    }
    if let Some(target) = matches.get_one::<String>("enqueue") {
        return Ok(Mode::Enqueue(target.clone().try_into()?));
    }
    if let Some(target) = matches.get_one::<String>("is-focused") {
        return Ok(Mode::IsFocused(target.clone().try_into()?));
    }
    Ok(Mode::Focus)
}

pub fn session_directory(
    platform: Platform,
    variable: impl Fn(&str) -> Option<OsString>,
) -> Result<PathBuf, Error> {
    let name = if platform == Platform::Darwin {
        "XDG_DATA_HOME"
    } else {
        "XDG_RUNTIME_DIR"
    };
    let root = match variable(name) {
        Some(root) => PathBuf::from(root),
        None => PathBuf::from(variable("HOME").ok_or(Error::MissingHome)?).join(".local/share"),
    };
    Ok(root.join("wrix/sessions"))
}

pub fn run<R: Runner>(
    mode: Mode,
    directory: &std::path::Path,
    platform: Platform,
    navigator: &Navigator<R>,
) -> Result<bool, Error> {
    match mode {
        Mode::Focus => attention::focus(directory, platform, navigator)?,
        Mode::Enqueue(target) => attention::enqueue(directory, &target, platform)?,
        Mode::Return => attention::return_to_origin(directory, platform, navigator)?,
        Mode::IsFocused(target) => {
            return Ok(attention::is_focused(
                directory, &target, platform, navigator,
            ));
        }
    }
    Ok(true)
}
