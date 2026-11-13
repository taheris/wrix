use std::fmt;
struct Failure(wrix_native_test::command::Error);
impl fmt::Debug for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}
fn main() -> Result<std::process::ExitCode, Failure> {
    wrix_native_test::command::run(std::env::args_os().skip(1)).map_err(Failure)?;
    Ok(std::process::ExitCode::SUCCESS)
}
