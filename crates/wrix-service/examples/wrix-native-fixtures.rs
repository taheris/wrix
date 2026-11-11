//! Explicitly built test infrastructure, never installed in production outputs.
mod native_fixture;

use std::{fmt, process::ExitCode};
struct Failure(native_fixture::Error);
impl fmt::Debug for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}
fn main() -> Result<ExitCode, Failure> {
    let mut args = std::env::args_os();
    let publisher = args.next().is_some_and(|program| {
        std::path::Path::new(&program)
            .file_name()
            .is_some_and(|name| name == "nix")
    });
    let mode = publisher.then(|| std::ffi::OsString::from("publisher-nix"));
    native_fixture::run(mode.into_iter().chain(args)).map_err(Failure)
}
