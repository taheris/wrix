//! Explicit host authority operations, independent of project launch configuration.

mod diagnostic;

use clap::{Arg, Command};
use std::{
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};
use wrix_state::{host::Host, store::Store};

pub const HELP: &str = "Manage workspace namespace admission.\n\nUsage: wrix workspace initialize\n       wrix workspace import-native --native-key <native-key>\n       wrix workspace acknowledge-scope --version 1\n       wrix workspace integration select --slot <slot> --executable <absolute-path>\n       wrix workspace integration disable\n       wrix workspace admit --integration <slot> --expect-membership <id> --checkout <absolute-directory> (--allocate | --retain <native-key>)\n       wrix workspace inspect --checkout <absolute-directory>\n       wrix workspace diagnose [--native-key <native-key>]\n";

fn parser() -> Command {
    Command::new("wrix workspace")
        .subcommand_required(true)
        .subcommand(Command::new("initialize").about("Initialize host admission state."))
        .subcommand(
            Command::new("diagnose")
                .about("Inspect host selection and retention metadata.")
                .arg(Arg::new("native-key").long("native-key")),
        )
        .subcommand(
            Command::new("acknowledge-scope")
                .about("Authorize standalone namespace allocation.")
                .arg(
                    Arg::new("version")
                        .long("version")
                        .required(true)
                        .value_parser(clap::value_parser!(u32)),
                ),
        )
        .subcommand(
            Command::new("integration")
                .about("Select external membership resolution.")
                .subcommand_required(true)
                .subcommand(
                    Command::new("select")
                        .about("Select a host workspace adapter.")
                        .arg(Arg::new("slot").long("slot").required(true))
                        .arg(
                            Arg::new("executable")
                                .long("executable")
                                .required(true)
                                .value_parser(clap::value_parser!(PathBuf)),
                        ),
                )
                .subcommand(
                    Command::new("disable")
                        .about("Disable resolution without releasing reservations."),
                ),
        )
        .subcommand(
            Command::new("import-native")
                .about("Import existing namespace metadata without binding it.")
                .arg(Arg::new("native-key").long("native-key").required(true)),
        )
        .subcommand(
            Command::new("admit")
                .about("Authorize a workspace namespace assignment.")
                .arg(Arg::new("integration").long("integration").required(true))
                .arg(
                    Arg::new("expect-membership")
                        .long("expect-membership")
                        .required(true),
                )
                .arg(
                    Arg::new("checkout")
                        .long("checkout")
                        .required(true)
                        .value_parser(clap::value_parser!(PathBuf)),
                )
                .arg(
                    Arg::new("allocate")
                        .long("allocate")
                        .action(clap::ArgAction::SetTrue)
                        .conflicts_with("retain"),
                )
                .arg(
                    Arg::new("retain")
                        .long("retain")
                        .required_unless_present("allocate"),
                ),
        )
        .subcommand(
            Command::new("inspect")
                .about("Inspect workspace admission without mutation.")
                .arg(
                    Arg::new("checkout")
                        .long("checkout")
                        .required(true)
                        .value_parser(clap::value_parser!(PathBuf)),
                ),
        )
}

pub fn run(
    args: &[std::ffi::OsString],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<ExitCode> {
    let matches = match parser().try_get_matches_from(
        std::iter::once(std::ffi::OsStr::new("wrix workspace"))
            .chain(args.iter().map(std::ffi::OsString::as_os_str)),
    ) {
        Ok(matches) => matches,
        Err(error) => {
            if error.use_stderr() {
                writeln!(stderr, "{error}")?;
                return Ok(ExitCode::FAILURE);
            }
            writeln!(stdout, "{error}")?;
            return Ok(ExitCode::SUCCESS);
        }
    };
    match apply(&matches) {
        Ok(message) => {
            writeln!(stdout, "{message}")?;
            Ok(ExitCode::SUCCESS)
        }
        Err(error) => {
            writeln!(stderr, "wrix workspace: {error}")?;
            writeln!(stderr, "executable: {}", diagnostic::executable())?;
            if let Ok(host) = Host::discover() {
                writeln!(
                    stderr,
                    "state_database: {}",
                    host.roots().database().display()
                )?;
            }
            Ok(ExitCode::FAILURE)
        }
    }
}

fn apply(matches: &clap::ArgMatches) -> Result<String, wrix_state::store::Error> {
    use wrix_state::store::Error;
    let host = Host::discover()?;
    match matches.subcommand() {
        Some(("diagnose", arguments)) => diagnostic::describe(
            &host,
            arguments
                .get_one::<String>("native-key")
                .map(String::as_str),
        ),
        Some(("initialize", _)) => {
            Store::initialize(&host)?;
            Ok("Host admission initialized.".into())
        }
        Some(("acknowledge-scope", arguments)) => {
            let version = arguments
                .get_one::<u32>("version")
                .ok_or(Error::Unsupported)?;
            Store::acknowledge(&host, *version)?;
            Ok(format!("Standalone scope policy {version} acknowledged."))
        }
        Some(("integration", arguments)) => match arguments.subcommand() {
            Some(("select", arguments)) => {
                let slot = arguments
                    .get_one::<String>("slot")
                    .ok_or(Error::Unsupported)?;
                let executable = arguments
                    .get_one::<PathBuf>("executable")
                    .ok_or(Error::Unsupported)?;
                let slot = wrix_state::integration::Slot::parse(slot)?;
                Store::select(&host, &slot, executable)?;
                Ok(format!(
                    "Integration selected: {slot}.\nstate_database: {}\nexecutable: {}",
                    host.roots().database().display(),
                    diagnostic::executable()
                ))
            }
            Some(("disable", _)) => {
                Store::disable(&host)?;
                Ok("Integration disabled; reservations retained.".into())
            }
            _ => Err(Error::Unsupported),
        },
        Some(("import-native", arguments)) => {
            let key = wrix_core::path::WorkspaceHash::parse(
                arguments
                    .get_one::<String>("native-key")
                    .ok_or(Error::Unsupported)?,
            )
            .map_err(|error| Error::Invalid(error.to_string()))?;
            Store::import_native(&host, &key)?;
            Ok(format!("Imported metadata for native key: {key}."))
        }
        Some(("admit", arguments)) => {
            let slot = arguments
                .get_one::<String>("integration")
                .ok_or(Error::Unsupported)?;
            let expected = wrix_state::resolver::Membership::parse(
                arguments
                    .get_one::<String>("expect-membership")
                    .ok_or(Error::Unsupported)?,
            )?;
            let caller = arguments
                .get_one::<PathBuf>("checkout")
                .ok_or(Error::Unsupported)?;
            if !caller.is_absolute() {
                return Err(Error::Invalid("checkout must be absolute".into()));
            }
            let intent = if let Some(key) = arguments.get_one::<String>("retain") {
                wrix_state::store::admission::Intent::Retain(
                    wrix_core::path::WorkspaceHash::parse(key)
                        .map_err(|error| Error::Invalid(error.to_string()))?,
                )
            } else {
                wrix_state::store::admission::Intent::Allocate
            };
            let slot = wrix_state::integration::Slot::parse(slot)?;
            let key = Store::admit(&host, &slot, &expected, caller, &intent)?;
            Ok(format!("Admitted native key: {key}."))
        }
        Some(("inspect", arguments)) => {
            let checkout = arguments
                .get_one::<PathBuf>("checkout")
                .ok_or(Error::Unsupported)?;
            if !checkout.is_absolute() {
                return Err(Error::Invalid("checkout must be absolute".into()));
            }
            let context = wrix_state::context::Context::inspect(&host, checkout)?;
            let store = Store::inspect(&host)?;
            Ok(format!(
                "caller: {}\nscope: {}\nnative_key: {}\nintegrated: {}\nstandalone_allocation_acknowledged: {}",
                context.caller().display(),
                context.primary().display(),
                context.key(),
                context.integrated(),
                store
                    .as_ref()
                    .map(Store::acknowledged)
                    .transpose()?
                    .unwrap_or(false)
            ))
        }
        _ => Err(Error::Unsupported),
    }
}
