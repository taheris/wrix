//! Read-only host selection and retention provenance; no credentials or native readiness claims.

use std::{env, fmt::Write, fs, os::unix::fs::MetadataExt};
use wrix_state::{
    host::Host,
    store::{Error, Store},
};

pub(super) fn executable() -> String {
    match env::current_exe() {
        Ok(path) => path.display().to_string(),
        Err(error) => format!("unavailable ({error})"),
    }
}

pub(super) fn describe(host: &Host, key: Option<&str>) -> Result<String, Error> {
    let mut report = format!(
        "executable: {}\nversion: {}\nstate_root: {}\ncache_root: {}\ndatabase: {}\n",
        executable(),
        env!("CARGO_PKG_VERSION"),
        host.roots().state().display(),
        host.roots().cache().display(),
        host.roots().database().display()
    );
    for name in ["HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME"] {
        let value = env::var_os(name).map_or_else(
            || "unset".into(),
            |value| value.to_string_lossy().into_owned(),
        );
        writeln!(report, "{name}: {value}").map_err(std::io::Error::other)?;
    }
    report.push_str("source_revision: not embedded; identify executable digest and source lock\n");
    let store = Store::inspect(host)?;
    let Some(store) = store else {
        report.push_str("admission: uninitialized (database absent)\n");
        return Ok(report);
    };
    let metadata = fs::symlink_metadata(host.roots().database())?;
    write!(report, "database_device: {}\ndatabase_inode: {}\ndatabase_uid: {}\nadmission_initialized: {}\nstandalone_scope_acknowledged: {}\nauthority_generation_bytes: {}\n",
        metadata.dev(), metadata.ino(), metadata.uid(), store.initialized()?,
        store.acknowledged()?, store.generation()?.as_bytes().len()).map_err(std::io::Error::other)?;
    match store.integration()? {
        Some((slot, executable)) => {
            write!(
                report,
                "integration: {slot}\nadapter: {}\n",
                executable.display()
            )
            .map_err(std::io::Error::other)?;
        }
        None => report.push_str("integration: disabled\n"),
    }
    if let Some(key) = key {
        let key = wrix_core::path::WorkspaceHash::parse(key)
            .map_err(|error| Error::Invalid(error.to_string()))?;
        Store::check_retention(host, &key)?;
        let legacy = host.roots().workspace_state(&key).join("keys/cache.secret");
        let legacy_present = match fs::symlink_metadata(legacy) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        write!(report, "native_key: {key}\nretention_metadata: supported\nlegacy_cache_secret_present: {legacy_present}\nlegacy_cache_secret: not read; not Celler signing authority\nactive_celler_key: not checked; use authenticated native service readiness\n").map_err(std::io::Error::other)?;
    }
    Ok(report)
}
