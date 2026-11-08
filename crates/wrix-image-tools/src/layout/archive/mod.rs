//! Streaming POSIX/PAX package archives with upstream-compatible headers.
use super::{Digest, Error, Layer, Provenance};
use sha2::{Digest as _, Sha256};
use std::{
    fs,
    io::{self, BufWriter, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt},
    },
    path::{Path, PathBuf},
};

pub(super) struct Ownership<'a> {
    pub uid: u64,
    pub gid: u64,
    pub uname: &'a str,
    pub gname: &'a str,
}
struct HashingWriter<'a> {
    file: &'a mut fs::File,
    hash: Sha256,
    size: u64,
}
impl Write for HashingWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.file.write(bytes)?;
        self.hash.update(&bytes[..written]);
        self.size += u64::try_from(written).map_err(io::Error::other)?;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Directory,
    Symlink,
    Fifo,
    Extended,
}
impl Kind {
    const fn byte(self) -> u8 {
        match self {
            Self::File => b'0',
            Self::Directory => b'5',
            Self::Symlink => b'2',
            Self::Fifo => b'6',
            Self::Extended => b'x',
        }
    }
}

struct Header<'a> {
    name: &'a [u8],
    mode: u32,
    kind: Kind,
    uid: u64,
    gid: u64,
    size: u64,
    mtime: i64,
    link: &'a [u8],
    uname: &'a [u8],
    gname: &'a [u8],
}

fn filesystem_points(mut bytes: &[u8]) -> impl Iterator<Item = u32> {
    std::iter::from_fn(move || {
        let first = *bytes.first()?;
        if first.is_ascii() {
            bytes = &bytes[1..];
            return Some(u32::from(first));
        }
        let width = match first {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => 1,
        };
        if (2..=4).contains(&width)
            && bytes.len() >= width
            && std::str::from_utf8(&bytes[..width]).is_ok()
        {
            let mut point = u32::from(first & (0x7f >> width));
            for byte in &bytes[1..width] {
                point = (point << 6) | u32::from(byte & 0x3f);
            }
            bytes = &bytes[width..];
            Some(point)
        } else {
            bytes = &bytes[1..];
            Some(0xdc00 + u32::from(first))
        }
    })
}

fn ascii_replace(bytes: &[u8]) -> Vec<u8> {
    filesystem_points(bytes)
        .map(|point| match u8::try_from(point) {
            Ok(byte) if byte.is_ascii() => byte,
            _ => b'?',
        })
        .collect()
}

fn filesystem_order(left: &Path, right: &Path) -> std::cmp::Ordering {
    let mut left = left.components();
    let mut right = right.components();
    loop {
        match (left.next(), right.next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(a), Some(b)) => {
                let order = filesystem_points(a.as_os_str().as_bytes())
                    .cmp(filesystem_points(b.as_os_str().as_bytes()));
                if !order.is_eq() {
                    return order;
                }
            }
        }
    }
}

fn string_field(field: &mut [u8], value: &[u8]) {
    let bytes = ascii_replace(value);
    let length = bytes.len().min(field.len());
    field[..length].copy_from_slice(&bytes[..length]);
}

fn numeric_field(field: &mut [u8], value: i128) {
    let width = field.len() - 1;
    let number = if value >= 0 && value < (1_i128 << (3 * width)) {
        value
    } else {
        0
    };
    let number = format!("{number:0width$o}");
    field[..width].copy_from_slice(number.as_bytes());
}

fn ustar(header: &Header<'_>) -> [u8; 512] {
    let mut block = [0; 512];
    string_field(&mut block[..100], header.name);
    numeric_field(&mut block[100..108], i128::from(header.mode & 0o7777));
    numeric_field(&mut block[108..116], i128::from(header.uid));
    numeric_field(&mut block[116..124], i128::from(header.gid));
    numeric_field(&mut block[124..136], i128::from(header.size));
    numeric_field(&mut block[136..148], i128::from(header.mtime));
    block[148..156].fill(b' ');
    block[156] = header.kind.byte();
    string_field(&mut block[157..257], header.link);
    block[257..265].copy_from_slice(b"ustar\x0000");
    string_field(&mut block[265..297], header.uname);
    string_field(&mut block[297..329], header.gname);
    let checksum: u64 = block.iter().map(|byte| u64::from(*byte)).sum();
    block[148..154].copy_from_slice(format!("{checksum:06o}").as_bytes());
    block[154] = 0;
    block
}

fn padding(output: &mut impl Write, bytes: u64, block: u64) -> io::Result<()> {
    let count = (block - bytes % block) % block;
    let zeros = [0; 10240];
    output.write_all(&zeros[..usize::try_from(count).map_err(io::Error::other)?])
}

fn header(output: &mut impl Write, header: &Header<'_>) -> io::Result<()> {
    let mut records: Vec<(&str, Vec<u8>)> = Vec::new();
    for (key, value, length) in [
        ("path", header.name, 100),
        ("linkpath", header.link, 100),
        ("uname", header.uname, 32),
        ("gname", header.gname, 32),
    ] {
        if !value.is_ascii() || value.len() > length {
            records.push((key, value.to_vec()));
        }
    }
    for (key, value, digits) in [
        ("uid", i128::from(header.uid), 8),
        ("gid", i128::from(header.gid), 8),
        ("size", i128::from(header.size), 12),
        ("mtime", i128::from(header.mtime), 12),
    ] {
        if value < 0 || value >= (1_i128 << (3 * (digits - 1))) {
            records.push((key, value.to_string().into_bytes()));
        }
    }
    if !records.is_empty() {
        let mut payload = Vec::new();
        if records
            .iter()
            .any(|(_, bytes)| std::str::from_utf8(bytes).is_err())
        {
            payload.extend_from_slice(b"21 hdrcharset=BINARY\n");
        }
        for (key, value) in records {
            let base = key.len() + value.len() + 3;
            let mut length = base + 1;
            loop {
                let next = base + length.to_string().len();
                if length == next {
                    break;
                }
                length = next;
            }
            payload.extend_from_slice(format!("{length} {key}=").as_bytes());
            payload.extend_from_slice(&value);
            payload.push(b'\n');
        }
        let pax = Header {
            name: b"././@PaxHeader",
            mode: 0,
            kind: Kind::Extended,
            uid: 0,
            gid: 0,
            size: u64::try_from(payload.len()).map_err(io::Error::other)?,
            mtime: 0,
            link: b"",
            uname: b"",
            gname: b"",
        };
        output.write_all(&ustar(&pax))?;
        output.write_all(&payload)?;
        padding(output, pax.size, 512)?;
    }
    output.write_all(&ustar(header))
}

fn paths(root: &Path, entries: &mut Vec<PathBuf>) -> io::Result<()> {
    entries.push(root.to_owned());
    if fs::symlink_metadata(root)?.is_dir() {
        for entry in fs::read_dir(root)? {
            paths(&entry?.path(), entries)?;
        }
    }
    Ok(())
}

fn entry(
    output: &mut impl Write,
    path: &Path,
    mtime: i64,
    owner: &Ownership<'_>,
) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let mut name = path.as_os_str().as_bytes().to_vec();
    let (kind, size, link) = if metadata.is_file() {
        (Kind::File, metadata.len(), Vec::new())
    } else if metadata.is_dir() {
        if !name.ends_with(b"/") {
            name.push(b'/');
        }
        (Kind::Directory, 0, Vec::new())
    } else if metadata.file_type().is_symlink() {
        (
            Kind::Symlink,
            0,
            fs::read_link(path)?.as_os_str().as_bytes().to_vec(),
        )
    } else if metadata.file_type().is_fifo() {
        (Kind::Fifo, 0, Vec::new())
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsupported Nix package file type",
        ));
    };
    header(
        output,
        &Header {
            name: &name,
            mode: metadata.mode(),
            kind,
            uid: owner.uid,
            gid: owner.gid,
            size,
            mtime,
            link: &link,
            uname: owner.uname.as_bytes(),
            gname: owner.gname.as_bytes(),
        },
    )?;
    if kind == Kind::File {
        use io::Read as _;
        let copied = io::copy(&mut fs::File::open(path)?.take(size), output)?;
        if copied != size {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "package changed while archiving",
            ));
        }
        padding(output, size, 512)?;
    }
    Ok(())
}

fn archive(
    output: &mut impl Write,
    roots: &[PathBuf],
    mtime: i64,
    owner: &Ownership<'_>,
) -> io::Result<()> {
    for name in [b"/nix/".as_slice(), b"/nix/store/".as_slice()] {
        header(
            output,
            &Header {
                name,
                mode: 0o755,
                kind: Kind::Directory,
                uid: owner.uid,
                gid: owner.gid,
                size: 0,
                mtime,
                link: b"",
                uname: owner.uname.as_bytes(),
                gname: owner.gname.as_bytes(),
            },
        )?;
    }
    for root in roots {
        if !root.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "package roots must be absolute",
            ));
        }
        let mut entries = Vec::new();
        paths(root, &mut entries)?;
        if entries
            .iter()
            .any(|path| path.as_os_str().to_str().is_none())
        {
            entries.sort_by(|left, right| filesystem_order(left, right));
        } else {
            entries.sort();
        }
        for path in entries {
            entry(output, &path, mtime, owner)?;
        }
    }
    Ok(())
}

pub(super) fn write(
    roots: &[PathBuf],
    blobs: &Path,
    mtime: i64,
    owner: &Ownership<'_>,
) -> Result<Layer, Error> {
    let mut temp = tempfile::NamedTempFile::new_in(blobs)?;
    let mut writer = BufWriter::with_capacity(
        65536,
        HashingWriter {
            file: temp.as_file_mut(),
            hash: Sha256::new(),
            size: 0,
        },
    );
    archive(&mut writer, roots, mtime, owner)?;
    writer.write_all(&[0; 1024])?;
    writer.flush()?;
    let length = writer.get_ref().size;
    padding(&mut writer, length, 10240)?;
    writer.flush()?;
    let writer = writer
        .into_inner()
        .map_err(std::io::IntoInnerError::into_error)?;
    let size = writer.size;
    let digest = Digest(format!("{:x}", writer.hash.finalize()));
    temp.persist(blobs.join(&digest.0))
        .map_err(|error| error.error)?;
    Ok(Layer {
        digest,
        size,
        paths: Provenance::Paths(roots.to_vec()),
    })
}

pub(super) fn append_bytes(
    output: &mut impl Write,
    name: &str,
    bytes: &[u8],
    mtime: i64,
) -> io::Result<()> {
    let size = u64::try_from(bytes.len()).map_err(io::Error::other)?;
    header(
        output,
        &Header {
            name: name.as_bytes(),
            mode: 0o644,
            kind: Kind::File,
            uid: 0,
            gid: 0,
            size,
            mtime,
            link: b"",
            uname: b"",
            gname: b"",
        },
    )?;
    output.write_all(bytes)?;
    padding(output, size, 512)
}

pub(super) fn append_blob(
    output: &mut impl Write,
    name: &str,
    source: &Path,
    size: u64,
    mtime: i64,
) -> io::Result<()> {
    use io::Read as _;
    let mut source = fs::File::open(source)?.take(size);
    header(
        output,
        &Header {
            name: name.as_bytes(),
            mode: 0o644,
            kind: Kind::File,
            uid: 0,
            gid: 0,
            size,
            mtime,
            link: b"",
            uname: b"",
            gname: b"",
        },
    )?;
    let copied = io::copy(&mut source, output)?;
    if copied != size {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "cached layer changed while exporting",
        ));
    }
    padding(output, size, 512)
}

pub(super) fn finish_export(output: &mut impl Write, size: u64) -> io::Result<()> {
    output.write_all(&[0; 1024])?;
    padding(
        output,
        size.checked_add(1024)
            .ok_or_else(|| io::Error::other("archive size overflow"))?,
        10240,
    )?;
    output.flush()
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn pax_headers_match_frozen_upstream_bytes_for_unicode_long_paths_and_numeric_overflow() {
        let name = format!("/nix/store/{}/café", "a".repeat(130));
        let uname = "buildér".repeat(6);
        let entry = Header {
            name: name.as_bytes(),
            mode: 0o751,
            kind: Kind::File,
            uid: 123_456_789,
            gid: 20,
            size: 5,
            mtime: -1,
            link: b"",
            uname: uname.as_bytes(),
            gname: b"staff",
        };
        let mut bytes = Vec::new();
        header(&mut bytes, &entry).unwrap();
        assert_eq!(
            bytes,
            include_bytes!("../../../tests/fixtures/oci-pax-header.bin")
        );
    }

    #[test]
    fn binary_names_use_surrogateescape_order_and_ascii_fallback() {
        use std::os::unix::ffi::OsStrExt as _;
        let binary = Path::new(std::ffi::OsStr::from_bytes(b"/root/\xff"));
        let emoji = Path::new("/root/😀");
        assert!(filesystem_order(binary, emoji).is_lt());
        assert_eq!(ascii_replace(b"\xe2\x82"), b"??");
        assert_eq!(ascii_replace("café".as_bytes()), b"caf?");
    }

    struct Failure {
        remaining: usize,
    }
    impl Write for Failure {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Err(io::Error::other("injected archive consumer failure"));
            }
            let written = bytes.len().min(self.remaining);
            self.remaining -= written;
            Ok(written)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn early_and_late_consumer_failures_propagate_without_a_worker_or_pipe() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("payload"), vec![b'x'; 4 * 1024 * 1024]).unwrap();
        let owner = Ownership {
            uid: 0,
            gid: 0,
            uname: "root",
            gname: "root",
        };
        for remaining in [0, 4096, 4 * 1024 * 1024] {
            let error = archive(
                &mut Failure { remaining },
                &[root.path().to_owned()],
                1,
                &owner,
            )
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("injected archive consumer failure")
            );
        }
    }
}
