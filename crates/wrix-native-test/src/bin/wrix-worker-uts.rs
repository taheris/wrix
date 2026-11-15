#[cfg(target_os = "linux")]
#[path = "worker_uts/mod.rs"]
mod worker_uts;
#[cfg(target_os = "linux")]
fn main() -> Result<(), worker_uts::Error> {
    worker_uts::run(&std::env::args_os().skip(1).collect::<Vec<_>>())
}
#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr().lock(), "worker UTS probe requires Linux");
    std::process::ExitCode::FAILURE
}
