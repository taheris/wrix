use std::fmt;
struct Failure(wrix_runtime_env::command::Error);
impl fmt::Debug for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}
fn main() -> Result<(), Failure> {
    wrix_runtime_env::command::run(std::env::args_os().skip(1)).map_err(Failure)
}
