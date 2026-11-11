use std::fmt;

struct Failure(wrix_verify_tools::command::Error);

impl fmt::Debug for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

fn main() -> Result<std::process::ExitCode, Failure> {
    match wrix_verify_tools::command::run(std::env::args_os().skip(1)) {
        Ok(()) => Ok(std::process::ExitCode::SUCCESS),
        Err(wrix_verify_tools::command::Error::NoMember) => Ok(std::process::ExitCode::FAILURE),
        Err(
            error @ wrix_verify_tools::command::Error::Image(
                wrix_verify_tools::artifact::image::command::Error::Usage,
            ),
        ) => {
            use std::io::Write;
            writeln!(std::io::stderr().lock(), "{error}")
                .map_err(|error| Failure(wrix_verify_tools::command::Error::Io(error)))?;
            Ok(std::process::ExitCode::from(2))
        }
        Err(error) => Err(Failure(error)),
    }
}
