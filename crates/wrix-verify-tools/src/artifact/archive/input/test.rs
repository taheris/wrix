use super::*;
use std::io::Write;

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn gzip_restart_seeks_to_the_validated_member_instead_of_replaying_the_prefix() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("archive.gz");
    let prefix = gzip(&vec![b'p'; 1024 * 1024]);
    let mut data = prefix.clone();
    data.extend_from_slice(&[0; 19]);
    let compressed = data.len() as u64;
    data.extend(gzip(b"target payload"));
    fs_write(&path, &data);
    let mut reader = open(&path).unwrap();
    let mut decoded = Vec::new();
    reader.read_to_end(&mut decoded).unwrap();
    let checkpoints = reader.checkpoints().to_vec();
    assert_eq!(checkpoints.len(), 2);
    assert_eq!(
        checkpoints[1],
        Checkpoint {
            compressed,
            uncompressed: 1024 * 1024
        }
    );
    // Only the indexed transport primitive bypasses the prefix. Image callers
    // additionally require the source identity observed during preparation.
    data[prefix.len() - 8] ^= 1;
    fs_write(&path, &data);
    let mut reader = open_at(&path, 1024 * 1024 + 7, &checkpoints).unwrap();
    assert_eq!(reader.checkpoints()[0], checkpoints[1]);
    let mut payload = Vec::new();
    reader.read_to_end(&mut payload).unwrap();
    assert_eq!(payload, b"payload");
    assert!(open_at(&path, 1024 * 1024 + 7, &[]).is_err());
}

fn fs_write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn raw_tar_offsets_seek_without_materializing_sparse_prefixes() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("archive.tar");
    let mut file = File::create(&path).unwrap();
    let offset = 64 * 1024 * 1024;
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(b"payload").unwrap();
    drop(file);
    let mut reader = open_at(&path, offset, &[]).unwrap();
    let mut payload = Vec::new();
    reader.read_to_end(&mut payload).unwrap();
    assert_eq!(payload, b"payload");
    assert_eq!(reader.checkpoints(), []);
    assert!(open_at(&path, offset + 8, &[]).is_err());
}

#[test]
fn arbitrarily_many_empty_members_keep_restart_memory_bounded() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("empty.gz");
    let member = gzip(b"");
    let mut bytes = member.repeat(8200);
    bytes.extend(gzip(b"retained"));
    fs_write(&path, &bytes);
    let mut reader = open(&path).unwrap();
    let mut data = Vec::new();
    reader.read_to_end(&mut data).unwrap();
    assert_eq!(data, b"retained");
    assert_eq!(reader.checkpoints().len(), 8192);
}
