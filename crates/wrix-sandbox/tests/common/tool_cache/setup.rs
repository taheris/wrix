use std::{fs, io::Write, path::Path, time::Duration};

use flate2::{Compression, write::GzEncoder};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use super::super::TestResult;

const CRATE_FILES: &[(&str, &[u8])] = &[
    (
        "Cargo.toml",
        b"[package]\nname = \"registrydemo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    ),
    ("src/lib.rs", b"pub fn value() -> u32 { 2 }\n"),
];
const WHEEL_FILES: &[(&str, &[u8])] = &[
    ("cache_demo.py", b"VALUE = 71\n"),
    (
        "cache_demo-1.0.dist-info/METADATA",
        b"Metadata-Version: 2.1\nName: cache-demo\nVersion: 1.0\n",
    ),
    (
        "cache_demo-1.0.dist-info/WHEEL",
        b"Wheel-Version: 1.0\nGenerator: fixture\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
    ),
    ("cache_demo-1.0.dist-info/RECORD", b""),
];

pub fn registry_archive(path: &Path) -> TestResult<String> {
    let gzip = GzEncoder::new(fs::File::create(path)?, Compression::default());
    let mut builder = tar::Builder::new(gzip);
    for &(name, bytes) in CRATE_FILES {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, format!("registrydemo-0.1.0/{name}"), bytes)?;
    }
    builder.into_inner()?.finish()?;
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}

pub fn wheel(path: &Path) -> TestResult {
    let mut writer = ZipWriter::new(fs::File::create(path)?);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o600);
    for &(name, bytes) in WHEEL_FILES {
        writer.start_file(name, options)?;
        writer.write_all(bytes)?;
    }
    writer.finish()?;
    Ok(())
}

pub fn assert_tracking(path: &Path) -> TestResult {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    assert_eq!(
        connection.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))?,
        "ok"
    );
    for query in [
        "SELECT count(*) FROM git_db",
        "SELECT count(*) FROM registry_src",
        "SELECT count(*) FROM registry_crate",
    ] {
        assert_eq!(
            connection.query_row(query, [], |row| row.get::<_, i64>(0))?,
            1,
            "{query}"
        );
    }
    assert!(
        connection.query_row("SELECT count(*) FROM git_checkout", [], |row| row
            .get::<_, i64>(0))?
            >= 1
    );
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use std::io::Read;

    #[test]
    fn registry_fixture_has_original_payload_modes_and_matching_compressed_hash() -> TestResult {
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().join("fixture.crate");
        let checksum = registry_archive(&path)?;
        assert_eq!(checksum, format!("{:x}", Sha256::digest(fs::read(&path)?)));
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(fs::File::open(path)?));
        let mut count = 0;
        for entry in archive.entries()? {
            let &(name, expected) = CRATE_FILES
                .get(count)
                .ok_or("unexpected crate fixture member")?;
            let mut entry = entry?;
            assert_eq!(
                entry.path()?.to_str(),
                Some(format!("registrydemo-0.1.0/{name}").as_str())
            );
            assert_eq!(entry.header().mode()?, 0o644);
            let mut contents = Vec::new();
            entry.read_to_end(&mut contents)?;
            assert_eq!(contents, expected);
            count += 1;
        }
        assert_eq!(count, 2);
        std::io::copy(&mut archive.into_inner(), &mut std::io::sink())?;
        Ok(())
    }

    #[test]
    fn wheel_fixture_has_original_four_files_and_exact_bytes() -> TestResult {
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().join("fixture.whl");
        wheel(&path)?;
        let mut archive = zip::ZipArchive::new(fs::File::open(path)?)?;
        assert_eq!(archive.len(), 4);
        for &(name, expected) in WHEEL_FILES {
            let mut entry = archive.by_name(name)?;
            assert_eq!(entry.compression(), CompressionMethod::Stored);
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            assert_eq!(bytes, expected);
        }
        Ok(())
    }

    #[test]
    fn sqlite_corruption_cannot_be_reported_as_valid_tracking() -> TestResult {
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().join("broken-cache");
        fs::write(&path, b"not a SQLite database")?;
        assert!(assert_tracking(&path).is_err());
        Ok(())
    }

    fn database(path: &Path) -> TestResult {
        let connection = Connection::open(path)?;
        for table in ["git_db", "git_checkout", "registry_src", "registry_crate"] {
            connection.execute_batch(&format!(
                "CREATE TABLE {table}(item TEXT); INSERT INTO {table} VALUES ('present');"
            ))?;
        }
        Ok(())
    }

    #[test]
    fn sqlite_tracking_assertions_accept_valid_and_reject_each_wrong_count() -> TestResult {
        let temporary = tempfile::tempdir()?;
        for (index, table) in ["git_db", "git_checkout", "registry_src", "registry_crate"]
            .iter()
            .enumerate()
        {
            let path = temporary.path().join(index.to_string());
            database(&path)?;
            assert_tracking(&path)?;
            Connection::open(&path)?.execute_batch(&format!("DELETE FROM {table}"))?;
            assert!(std::panic::catch_unwind(|| assert_tracking(&path)).is_err());
        }
        Ok(())
    }
}
