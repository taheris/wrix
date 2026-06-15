use std::process::ExitCode;

fn main() -> ExitCode {
    wrix_cache::helper::main(wrix_cache::helper::Helper::Publish)
}
