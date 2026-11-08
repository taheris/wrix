//! Native controller fixture for host-local clients, independent of vmnet availability.
use std::{num::NonZeroUsize, path::PathBuf};
use wrix_service_server::control::{Controller, serving::Host};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let mut args = std::env::args_os().skip(1);
    let root = PathBuf::from(args.next().ok_or("missing controller root")?);
    if args.next().is_some() {
        return Err("unexpected controller argument".into());
    }
    if root.join("stopped").try_exists()? {
        return Ok(());
    }
    let issuer = root
        .parent()
        .ok_or("missing controller parent")?
        .join("host-control/celler-issuer");
    let controller = Controller::bind(&root.join("authority"))?.with_cache_issuer(&issuer)?;
    let host = Host::bind_loopback(controller, NonZeroUsize::MIN.saturating_add(1023)).await?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    host.serve(async move {
        terminate.recv().await;
    })
    .await?;
    Ok(())
}
