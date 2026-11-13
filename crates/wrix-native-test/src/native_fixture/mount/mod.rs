use std::{
    ffi::OsString,
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// malformed native mount table
    Framing,
    /// malformed native mount escape
    Escape,
    /// unexpected fixture export location: {0}
    Export(PathBuf),
    /// fixture still has non-administrative namespace mounts: {0:?}
    Remaining(Vec<PathBuf>),
}

#[derive(Debug, Eq, PartialEq)]
pub struct Entry {
    pub target: PathBuf,
    pub kind: Vec<u8>,
    pub source: Vec<u8>,
}

fn unescape(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let mut output = Vec::with_capacity(bytes.len());
    let mut offset = 0;
    while offset < bytes.len() {
        if bytes[offset] != b'\\' {
            output.push(bytes[offset]);
            offset += 1;
            continue;
        }
        let digits = bytes.get(offset + 1..offset + 4).ok_or(Error::Escape)?;
        if !digits.iter().all(|digit| (b'0'..=b'7').contains(digit)) {
            return Err(Error::Escape);
        }
        let value = u16::from(digits[0] - b'0') * 64
            + u16::from(digits[1] - b'0') * 8
            + u16::from(digits[2] - b'0');
        output.push(u8::try_from(value).map_err(|_| Error::Escape)?);
        offset += 4;
    }
    Ok(output)
}

pub fn parse(bytes: &[u8]) -> Result<Vec<Entry>, Error> {
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            let fields: Vec<_> = line
                .split(u8::is_ascii_whitespace)
                .filter(|field| !field.is_empty())
                .collect();
            let separator = fields
                .iter()
                .position(|field| *field == b"-")
                .ok_or(Error::Framing)?;
            if separator < 6 || fields.len() < separator + 4 {
                return Err(Error::Framing);
            }
            Ok(Entry {
                target: PathBuf::from(OsString::from_vec(unescape(fields[4])?)),
                kind: fields[separator + 1].to_vec(),
                source: fields[separator + 2].to_vec(),
            })
        })
        .collect()
}

pub fn source(bytes: &[u8]) -> bool {
    bytes.strip_prefix(b"wrix-nix-").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    })
}

pub fn views(root: &Path, table: &[Entry]) -> Result<Vec<PathBuf>, Error> {
    let mut selected = Vec::new();
    for entry in table {
        if entry.target.starts_with(root) && entry.kind == b"fuse.sshfs" && source(&entry.source) {
            let relative = entry
                .target
                .strip_prefix(root)
                .map_err(|_| Error::Export(entry.target.clone()))?;
            let fields: Vec<_> = relative.iter().collect();
            let prefix = ["home", ".local", "state", "wrix", "control", "nix"];
            if fields.len() != 11
                || !fields
                    .iter()
                    .take(6)
                    .zip(prefix)
                    .all(|(actual, expected)| *actual == expected)
                || fields[8] != "executions"
                || fields[10] != "store"
            {
                return Err(Error::Export(entry.target.clone()));
            }
            selected.push(entry.target.clone());
        }
    }
    Ok(selected)
}

pub fn namespace_clear(root: &Path, table: &[Entry]) -> Result<(), Error> {
    let storage = root.join("home/.local/share/containers/storage");
    let runtime = root.join("runtime");
    let remaining: Vec<_> = table
        .iter()
        .filter(|entry| {
            entry.target.starts_with(root)
                && (entry.kind.starts_with(b"fuse.")
                    || !(entry.target == storage
                        || (entry.target == storage.join("overlay") && entry.kind != b"overlay")
                        || entry.target.starts_with(&runtime)))
        })
        .map(|entry| entry.target.clone())
        .collect();
    if remaining.is_empty() {
        Ok(())
    } else {
        Err(Error::Remaining(remaining))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    fn line(target: &Path, kind: &str, source: &str) -> Vec<u8> {
        let mut bytes = b"90 2 0:99 / ".to_vec();
        for byte in target.as_os_str().as_bytes() {
            match byte {
                b' ' => bytes.extend(b"\\040"),
                b'\\' => bytes.extend(b"\\134"),
                _ => bytes.push(*byte),
            }
        }
        bytes.extend(format!(" ro,nosuid,nodev - {kind} {source} ro\n").as_bytes());
        bytes
    }

    #[test]
    fn mountinfo_preserves_spaces_backslashes_and_non_utf8_bytes() {
        let target = PathBuf::from(OsString::from_vec(b"/private/view \\ \xff".to_vec()));
        let records = parse(&line(
            &target,
            "fuse.sshfs",
            &format!("wrix-nix-{}", "a".repeat(32)),
        ))
        .unwrap();
        assert_eq!(records[0].target, target);
        assert!(source(&records[0].source));
    }

    #[test]
    fn malformed_framing_and_out_of_range_escapes_fail_closed() {
        for bytes in [
            b"short".as_slice(),
            b"1 2 0:1 / /bad\\7 x - ext4 dev rw",
            b"1 2 0:1 / /bad\\777 x - ext4 dev rw",
        ] {
            assert!(parse(bytes).is_err());
        }
    }

    #[test]
    fn exports_require_both_owned_root_and_original_layout() {
        let root = Path::new("/owned");
        let view = root
            .join("home/.local/state/wrix/control/nix")
            .join("b".repeat(32))
            .join("x86_64-linux/executions")
            .join("c".repeat(64))
            .join("store");
        let source = format!("wrix-nix-{}", "a".repeat(32));
        let mut records = parse(&line(&view, "fuse.sshfs", &source)).unwrap();
        records.extend(parse(&line(Path::new("/peer/export"), "fuse.sshfs", &source)).unwrap());
        assert_eq!(views(root, &records).unwrap(), vec![view]);
        records
            .extend(parse(&line(Path::new("/owned/unexpected"), "fuse.sshfs", &source)).unwrap());
        assert!(matches!(views(root, &records), Err(Error::Export(_))));
    }

    #[test]
    fn only_administrative_storage_and_runtime_mounts_allow_release() {
        let root = Path::new("/owned");
        let storage = root.join("home/.local/share/containers/storage");
        for target in [
            &storage,
            &storage.join("overlay"),
            &root.join("runtime/containers"),
        ] {
            namespace_clear(root, &parse(&line(target, "ext4", "/dev/root")).unwrap()).unwrap();
        }
        for (target, kind) in [
            (storage.join("overlay"), "overlay"),
            (storage.join("overlay/layer/merged"), "overlay"),
            (storage.join("overlay"), "fuse.overlayfs"),
            (root.join("unknown"), "ext4"),
        ] {
            assert!(matches!(
                namespace_clear(root, &parse(&line(&target, kind, "other")).unwrap()),
                Err(Error::Remaining(_))
            ));
        }
    }
}
