use std::fmt;

struct Failure(wrix_verify_tools::command::Error);

impl fmt::Debug for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

fn main() -> Result<(), Failure> {
    wrix_verify_tools::command::run(std::env::args_os().skip(1)).map_err(Failure)
}
