//! Read-only, bounded Nix fetcher-cache snapshots, never service database maintenance.
use std::{fs, io, path::Path, time::Duration};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// expected a Nix fetcher-cache-vNUMBER.sqlite file
    Name,
    /// snapshot destination already exists
    Exists,
    /// snapshot I/O failed: {0}
    Io(#[from] io::Error),
    /// snapshot SQLite operation failed: {0}
    Sql(#[from] rusqlite::Error),
    /// snapshot path is not UTF-8
    Encoding,
}

pub fn snapshot(source: &Path, destination: &Path) -> Result<(), Error> {
    copy(source, destination, Duration::from_secs(10))
}

fn copy(source: &Path, destination: &Path, timeout: Duration) -> Result<(), Error> {
    let version = source
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("fetcher-cache-v"))
        .and_then(|name| name.strip_suffix(".sqlite"));
    if !version
        .is_some_and(|version| !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(Error::Name);
    }
    match destination.symlink_metadata() {
        Ok(_) => return Err(Error::Exists),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let parent = destination.parent().ok_or(Error::Encoding)?;
    let temporary = tempfile::NamedTempFile::new_in(parent)?;
    let source =
        rusqlite::Connection::open_with_flags(source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    source.busy_timeout(timeout)?;
    source.execute(
        "VACUUM INTO ?1",
        [temporary.path().to_str().ok_or(Error::Encoding)?],
    )?;
    fs::File::open(temporary.path())?.sync_all()?;
    temporary
        .persist_noclobber(destination)
        .map_err(|failure| Error::Io(failure.error))?;
    Ok(())
}

#[cfg(test)]
mod test;
