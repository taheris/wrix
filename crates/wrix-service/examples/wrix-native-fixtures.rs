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
    native_fixture::run(std::env::args_os().skip(1)).map_err(Failure)
}
