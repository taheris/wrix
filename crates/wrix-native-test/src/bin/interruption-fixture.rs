//! Disposable external runtime stubs, never installed in production Wrix outputs.
use wrix_native_test::native_fixture::runtime;

use std::{ffi::OsString, fmt, path::Path, process::ExitCode};
struct Failure(runtime::Error);
impl fmt::Debug for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}
fn main() -> Result<ExitCode, Failure> {
    let mut args = std::env::args_os();
    let program = args.next().ok_or(Failure(runtime::Error::Usage))?;
    let name = Path::new(&program)
        .file_name()
        .ok_or(Failure(runtime::Error::Usage))?;
    let aliases = ["native", "podman", "cat", "readlink", "fusermount3", "git"];
    let args: Vec<OsString> = if aliases.iter().any(|alias| name == *alias) {
        std::iter::once(name.to_owned()).chain(args).collect()
    } else {
        if args.next().as_deref() != Some(std::ffi::OsStr::new("interruption-runtime")) {
            return Err(Failure(runtime::Error::Usage));
        }
        args.collect()
    };
    runtime::run(&args).map_err(Failure)
}
