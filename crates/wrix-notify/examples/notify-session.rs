use std::{os::unix::process::CommandExt, process::Command};

#[derive(Debug, displaydoc::Display, thiserror::Error)]
enum Error {
    /// creating the notification fixture session: {0}
    Session(#[from] rustix::io::Errno),
    /// executing the packaged notification daemon: {0}
    Exec(#[from] std::io::Error),
}

fn main() -> Result<(), Error> {
    rustix::process::setsid()?;
    Err(Command::new("wrix-notifyd").exec().into())
}
