//! Bounded sequential tar indexing and validated multi-member gzip transport.

use std::collections::BTreeMap;
use std::io::{self, Read};

mod input;
pub use input::{Checkpoint, MAX_GZIP_CHECKPOINTS, open, open_at};

use super::text::Text;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub name: Text,
    pub kind: u8,
    pub mode: u32,
    pub size: u64,
    pub offset: u64,
    pub offset_data: u64,
    pub link: Text,
    pub sparse: Option<Vec<(u64, u64)>>,
}

impl Header {
    pub const fn regular(&self) -> bool {
        matches!(self.kind, 0 | b'0' | b'7' | b'S')
    }
}

struct Stream<R> {
    reader: R,
    position: u64,
}

impl<R: Read> Read for Stream<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.reader.read(bytes)?;
        self.position = self
            .position
            .checked_add(u64::try_from(count).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("archive offset overflow"))?;
        Ok(count)
    }
}

fn number(field: &[u8]) -> io::Result<u64> {
    if field.first().is_some_and(|byte| byte & 0x80 != 0) {
        if field[0] & 0x40 != 0 {
            return Err(io::Error::other("negative tar size or offset"));
        }
        let mut value = u64::from(field[0] & 0x7f);
        for byte in &field[1..] {
            value = value
                .checked_mul(256)
                .and_then(|value| value.checked_add(u64::from(*byte)))
                .ok_or_else(|| io::Error::other("tar number overflow"))?;
        }
        return Ok(value);
    }
    let text = std::str::from_utf8(field)
        .map_err(io::Error::other)?
        .trim_matches(['\0', ' ']);
    if text.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(text, 8).map_err(io::Error::other)
}

fn terminated(field: &[u8]) -> &[u8] {
    &field[..field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len())]
}

fn block<R: Read>(stream: &mut Stream<R>) -> io::Result<Option<[u8; 512]>> {
    let mut bytes = [0; 512];
    let count = stream.read(&mut bytes[..1])?;
    if count == 0 {
        return Ok(None);
    }
    stream.read_exact(&mut bytes[1..])?;
    Ok(Some(bytes))
}

pub fn drain(reader: &mut impl Read) -> io::Result<u64> {
    io::copy(reader, &mut io::sink())
}

fn skip(reader: &mut impl Read, count: u64) -> io::Result<()> {
    if drain(&mut reader.take(count))? != count {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "truncated archive payload",
        ));
    }
    Ok(())
}

fn pax(bytes: &[u8]) -> io::Result<BTreeMap<Vec<u8>, Vec<u8>>> {
    let mut values = BTreeMap::new();
    let mut offsets = Vec::new();
    let mut lengths = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        let space = bytes[start..]
            .iter()
            .position(|byte| *byte == b' ')
            .ok_or_else(|| io::Error::other("invalid PAX record length"))?
            + start;
        let length: usize = std::str::from_utf8(&bytes[start..space])
            .map_err(io::Error::other)?
            .parse()
            .map_err(io::Error::other)?;
        let end = start
            .checked_add(length)
            .filter(|end| *end <= bytes.len() && *end > space + 1)
            .ok_or_else(|| io::Error::other("invalid PAX record bounds"))?;
        let record = &bytes[space + 1..end];
        if record.last() != Some(&b'\n') {
            return Err(io::Error::other("invalid PAX record terminator"));
        }
        let equal = record
            .iter()
            .position(|byte| *byte == b'=')
            .ok_or_else(|| io::Error::other("invalid PAX record"))?;
        let key = &record[..equal];
        let value = &record[equal + 1..record.len() - 1];
        if key == b"GNU.sparse.offset" {
            offsets.push(decimal(value)?);
        }
        if key == b"GNU.sparse.numbytes" {
            lengths.push(decimal(value)?);
        }
        values.insert(key.to_vec(), value.to_vec());
        start = end;
    }
    if values.contains_key(b"GNU.sparse.size".as_slice())
        && (!offsets.is_empty() || !lengths.is_empty())
    {
        if offsets.len() != lengths.len() {
            return Err(io::Error::other("unpaired sparse map records"));
        }
        let map = offsets
            .into_iter()
            .zip(lengths)
            .flat_map(|(offset, size)| [offset.to_string(), size.to_string()])
            .collect::<Vec<_>>()
            .join(",");
        values.insert(b"GNU.sparse.map".to_vec(), map.into_bytes());
    }
    Ok(values)
}

fn decimal(bytes: &[u8]) -> io::Result<u64> {
    std::str::from_utf8(bytes)
        .map_err(io::Error::other)?
        .parse()
        .map_err(io::Error::other)
}

fn sparse_blocks(bytes: &[u8]) -> io::Result<Vec<(u64, u64)>> {
    bytes
        .as_chunks::<24>()
        .0
        .iter()
        .map(|chunk| Ok((number(&chunk[..12])?, number(&chunk[12..])?)))
        .collect()
}

fn sparse_line(reader: &mut impl Read) -> io::Result<u64> {
    let mut digits = Vec::new();
    loop {
        let mut byte = [0];
        reader.read_exact(&mut byte)?;
        if byte[0] == b'\n' {
            return decimal(&digits);
        }
        if !byte[0].is_ascii_digit() || digits.len() >= 20 {
            return Err(io::Error::other("invalid sparse map integer"));
        }
        digits.push(byte[0]);
    }
}

/// Visits payloads in place; complete transport draining validates late gzip trailers.
pub fn scan<R: Read>(
    reader: R,
    mut visit: impl FnMut(&Header, &mut dyn Read) -> io::Result<()>,
) -> io::Result<()> {
    let mut stream = Stream {
        reader,
        position: 0,
    };
    let mut global = BTreeMap::new();
    let mut local = BTreeMap::new();
    let mut long_name = None;
    let mut long_link = None;
    let mut extended_offset = None;
    loop {
        let offset = stream.position;
        let Some(bytes) = block(&mut stream)? else {
            if stream.position == 0 {
                return Err(io::Error::other("empty tar archive"));
            }
            break;
        };
        if bytes == [0; 512] {
            break;
        }
        let checksum = number(&bytes[148..156])?;
        let unsigned: u64 = bytes
            .iter()
            .enumerate()
            .map(|(index, byte)| {
                if (148..156).contains(&index) {
                    32
                } else {
                    u64::from(*byte)
                }
            })
            .sum();
        let signed: i64 = bytes
            .iter()
            .enumerate()
            .map(|(index, byte)| {
                if (148..156).contains(&index) {
                    32
                } else {
                    i64::from(i8::from_ne_bytes([*byte]))
                }
            })
            .sum();
        if checksum != unsigned && i64::try_from(checksum).map_err(io::Error::other)? != signed {
            return Err(io::Error::other("invalid tar header checksum"));
        }
        let stored_size = number(&bytes[124..136])?;
        let kind = bytes[156];
        if matches!(kind, b'x' | b'g' | b'L' | b'K') {
            if stored_size > 16 * 1024 * 1024 {
                return Err(io::Error::other("oversized tar extended metadata"));
            }
            let mut data = vec![0; usize::try_from(stored_size).map_err(io::Error::other)?];
            stream.read_exact(&mut data)?;
            skip(&mut stream, (512 - stored_size % 512) % 512)?;
            if kind != b'g' {
                extended_offset.get_or_insert(offset);
            }
            match kind {
                b'x' => local.extend(pax(&data)?),
                b'g' => global.extend(pax(&data)?),
                b'L' => long_name = Some(Text::from_bytes(terminated(&data))),
                b'K' => long_link = Some(Text::from_bytes(terminated(&data))),
                _ => {}
            }
            continue;
        }
        let mut attributes = global.clone();
        attributes.append(&mut local);
        let mut name = terminated(&bytes[..100]).to_vec();
        if bytes[257..263] == *b"ustar\0" && !terminated(&bytes[345..500]).is_empty() {
            let mut prefixed = terminated(&bytes[345..500]).to_vec();
            prefixed.push(b'/');
            prefixed.extend(name);
            name = prefixed;
        }
        let mut header = Header {
            name: long_name.take().unwrap_or_else(|| Text::from_bytes(&name)),
            kind,
            mode: u32::try_from(number(&bytes[100..108])?).map_err(io::Error::other)?,
            size: stored_size,
            offset: extended_offset.take().unwrap_or(offset),
            offset_data: stream.position,
            link: long_link
                .take()
                .unwrap_or_else(|| Text::from_bytes(terminated(&bytes[157..257]))),
            sparse: None,
        };
        if kind == b'5' {
            header.name.trim_directory_slashes();
        }
        if let Some(value) = attributes.get(b"path".as_slice()) {
            header.name = Text::from_bytes(value);
            header.name.trim_directory_slashes();
        }
        if let Some(value) = attributes.get(b"linkpath".as_slice()) {
            header.link = Text::from_bytes(value);
        }
        if let Some(value) = attributes.get(b"GNU.sparse.name".as_slice()) {
            header.name = Text::from_bytes(value);
        }
        if let Some(value) = attributes.get(b"size".as_slice()) {
            header.size = decimal(value)?;
        }
        let mut payload_size = header.size;
        let padded_size = payload_size;
        if kind == b'S' {
            let mut map = sparse_blocks(&bytes[386..482])?;
            let mut extended = bytes[482] != 0;
            while extended {
                let extra = block(&mut stream)?
                    .ok_or_else(|| io::Error::other("truncated GNU sparse header"))?;
                map.extend(
                    sparse_blocks(&extra[..504])?
                        .into_iter()
                        .filter(|&(offset, size)| offset != 0 && size != 0),
                );
                extended = extra[504] != 0;
            }
            header.size = number(&bytes[483..495])?;
            header.offset_data = stream.position;
            header.sparse = Some(map);
        }
        for key in [b"GNU.sparse.size".as_slice(), b"GNU.sparse.realsize"] {
            if let Some(value) = attributes.get(key) {
                header.size = decimal(value)?;
            }
        }
        if let Some(value) = attributes.get(b"GNU.sparse.map".as_slice()) {
            let numbers: Vec<_> = value
                .split(|byte| *byte == b',')
                .map(decimal)
                .collect::<io::Result<_>>()?;
            if numbers.len() % 2 != 0 {
                return Err(io::Error::other("odd sparse map"));
            }
            header.sparse = Some(
                numbers
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| (*pair).into())
                    .collect(),
            );
        }
        if attributes
            .get(b"GNU.sparse.major".as_slice())
            .is_some_and(|value| value == b"1")
            && attributes
                .get(b"GNU.sparse.minor".as_slice())
                .is_some_and(|value| value == b"0")
        {
            let map_start = stream.position;
            let count = sparse_line(&mut stream)?;
            if count > 1_000_000 {
                return Err(io::Error::other("oversized sparse map"));
            }
            let mut map = Vec::new();
            for _ in 0..count {
                map.push((sparse_line(&mut stream)?, sparse_line(&mut stream)?));
            }
            let consumed = stream.position - map_start;
            let padding = (512 - consumed % 512) % 512;
            skip(&mut stream, padding)?;
            let consumed = stream.position - map_start;
            payload_size = payload_size
                .checked_sub(consumed)
                .ok_or_else(|| io::Error::other("sparse map exceeds payload"))?;
            header.offset_data = stream.position;
            header.sparse = Some(map);
        }
        header.name.normalized()?;
        if let Some(map) = &header.sparse {
            let mut end = 0;
            let mut total = 0_u64;
            for &(start, length) in map {
                if length == 0 {
                    continue;
                }
                let next = start
                    .checked_add(length)
                    .ok_or_else(|| io::Error::other("sparse extent overflow"))?;
                if start < end || next > header.size {
                    return Err(io::Error::other("sparse extent outside logical file"));
                }
                end = next;
                total = total
                    .checked_add(length)
                    .ok_or_else(|| io::Error::other("sparse length overflow"))?;
            }
            if total > payload_size {
                return Err(io::Error::other("sparse extents exceed stored payload"));
            }
        }
        let mut payload = (&mut stream).take(payload_size);
        visit(&header, &mut payload)?;
        drain(&mut payload)?;
        if payload.limit() != 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated tar member",
            ));
        }
        skip(&mut stream, (512 - padded_size % 512) % 512)?;
    }
    drain(&mut stream)?;
    Ok(())
}

pub fn index(reader: impl Read) -> io::Result<BTreeMap<Text, Header>> {
    let mut entries = BTreeMap::new();
    scan(reader, |header, _| {
        if !header.kind.is_ascii() {
            return Err(io::Error::other("non-ASCII tar entry type"));
        }
        entries.insert(header.name.normalized()?, header.clone());
        Ok(())
    })?;
    Ok(entries)
}

#[cfg(test)]
mod test {
    use super::*;
    use std::io::Write;

    fn gzip(bytes: &[u8]) -> impl Read + '_ {
        input::Gzip::new(std::io::Cursor::new(bytes), 0).unwrap()
    }

    pub fn tar(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, content) in entries {
            let mut header = tar::Header::new_gnu();
            header.as_mut_bytes()[..name.len()].copy_from_slice(name);
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append(&header, *content).unwrap();
        }
        builder.into_inner().unwrap()
    }

    #[test]
    fn byte_names_offsets_and_normalized_last_header_win() {
        let data = tar(&[(b"./a/\xff", b"old"), (b"a/\xff", b"new")]);
        let entries = index(data.as_slice()).unwrap();
        let entry = entries.get(&Text::from_bytes(b"a/\xff")).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entry.offset, 1024);
        assert_eq!(entry.offset_data, 1536);
        assert_eq!(&data[1536..1539], b"new");
    }

    #[test]
    fn multimember_gzip_header_splits_and_late_crc_failures() {
        let data = tar(&[(b"payload", b"hello")]);
        let mut compressed = Vec::new();
        for chunk in data.chunks(37) {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(chunk).unwrap();
            compressed.extend(encoder.finish().unwrap());
        }
        assert_eq!(index(gzip(&compressed)).unwrap().len(), 1);
        let end = compressed.len();
        compressed[end - 8] ^= 1;
        assert!(index(gzip(&compressed)).is_err());
    }

    #[test]
    fn gzip_zero_padding_between_members_and_after_the_last_trailer_is_accepted() {
        let data = tar(&[(b"payload", b"hello")]);
        let mut compressed = Vec::new();
        for chunk in data.chunks(529) {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(chunk).unwrap();
            compressed.extend(encoder.finish().unwrap());
            compressed.extend_from_slice(&[0; 17]);
        }
        assert_eq!(index(gzip(&compressed)).unwrap().len(), 1);
        compressed.push(1);
        assert!(index(gzip(&compressed)).is_err());
    }

    #[test]
    fn non_ascii_inner_type_is_rejected_like_the_original_metadata_encoder() {
        let mut data = tar(&[(b"invalid-type", b"body")]);
        data[156] = 255;
        data[148..156].fill(b' ');
        let checksum: u64 = data[..512].iter().map(|byte| u64::from(*byte)).sum();
        data[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
        assert!(
            index(data.as_slice())
                .unwrap_err()
                .to_string()
                .contains("non-ASCII tar entry type")
        );
    }

    #[test]
    fn truncated_payload_and_unsafe_header_are_rejected() {
        assert!(index(&tar(&[(b"a", b"hello")])[..514]).is_err());
        assert!(index(tar(&[(b"../escape", b"bad")]).as_slice()).is_err());
    }
}
