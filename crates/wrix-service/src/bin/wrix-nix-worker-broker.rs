//! Private package-bound one-shot native worker transport.
use std::process::ExitCode;
fn main() -> ExitCode {
    match wrix_service::nix_store::owner::backend::client::bridge::broker::main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            use std::io::Write;
            if let Err(write) = writeln!(std::io::stderr(), "native worker broker failed: {error}")
            {
                tracing::error!(operation = "apple_nix_broker", %write, "cannot report failure");
            }
            ExitCode::FAILURE
        }
    }
}
