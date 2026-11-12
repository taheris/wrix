//! Verify exactly one retained payload occurrence across every emitted layer.
use super::{Image, Text, bounded, path};
use std::{
    fs::File,
    io::{self, Read},
    os::unix::ffi::OsStrExt,
    path::Path,
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// image payload I/O failed: {0}
    Io(#[from] io::Error),
    /// image payload contract failed: {0}
    Contract(&'static str),
}
fn normalized(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !matches!(byte, b'.' | b'/'))
        .unwrap_or(bytes.len());
    &bytes[start..]
}
fn reader(image: &Image, layer: &Text) -> Result<Box<dyn Read>, Error> {
    let (reader, size): (Box<dyn Read>, _) = if let Some(source) = &image.source {
        let &(offset, size) = source
            .locations
            .get(layer)
            .ok_or(Error::Contract("archive layer location missing"))?;
        (Box::new(image.archive_at(&source.path, offset)?), size)
    } else {
        let file = File::open(image.root.join(path(layer)?))?;
        let size = file.metadata()?.len();
        (Box::new(file), size)
    };
    Ok(Box::new(reader.take(size)))
}

pub fn run(source: &Path, expected: &Path) -> Result<(), Error> {
    let temporary = tempfile::tempdir()?;
    let image = Image::prepare(source, &temporary.path().join("image"))?;
    let target = normalized(expected.as_os_str().as_bytes());
    let mut found = Vec::new();
    for layer in &image.layers {
        let mut archive = tar::Archive::new(reader(&image, layer)?);
        for entry in archive.entries()? {
            let entry = entry?;
            if normalized(&entry.path_bytes()) == target {
                found.push(layer.clone());
            }
        }
    }
    if found.len() != 1 {
        return Err(Error::Contract(
            "expected exactly one payload occurrence across all layers",
        ));
    }
    let layer = Image::with_layers(&image.root, found)?;
    let output = temporary.path().join("payload");
    if !layer.extract(&Text::from_bytes(target), &output)? {
        return Err(Error::Contract("payload is not extractable"));
    }
    let mut expected = File::open(expected)?;
    let size = expected.metadata()?.len();
    let mut output = File::open(output)?;
    if output.metadata()?.len() != size
        || bounded(&mut output, size)? != bounded(&mut expected, size)?
    {
        return Err(Error::Contract("payload bytes differ"));
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use serde_json::json;
    use std::fs;
    fn append(writer: &mut tar::Builder<impl io::Write>, name: &Path, bytes: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        writer.append_data(&mut header, name, bytes).unwrap();
    }
    fn check(layers: &[usize], wrong: bool) -> Result<(), Error> {
        let temporary = tempfile::tempdir().unwrap();
        let payload = temporary.path().join("payload");
        fs::write(&payload, b"retained content").unwrap();
        let output = temporary.path().join("image.tar");
        let mut archive = tar::Builder::new(File::create(&output).unwrap());
        let names = (0..layers.len())
            .map(|index| format!("layer{index}.tar"))
            .collect::<Vec<_>>();
        append(
            &mut archive,
            Path::new("manifest.json"),
            &serde_json::to_vec(&json!([{"Config":"config.json","Layers":names}])).unwrap(),
        );
        append(&mut archive, Path::new("config.json"), b"{}");
        for (index, &count) in layers.iter().enumerate() {
            let mut layer = tar::Builder::new(Vec::new());
            for _ in 0..count {
                append(
                    &mut layer,
                    Path::new(std::ffi::OsStr::from_bytes(normalized(
                        payload.as_os_str().as_bytes(),
                    ))),
                    if wrong { b"wrong" } else { b"retained content" },
                );
            }
            append(
                &mut archive,
                Path::new(&names[index]),
                &layer.into_inner().unwrap(),
            );
        }
        archive.finish().unwrap();
        run(&output, &payload)
    }
    #[test]
    fn real_nested_archive_retains_exact_payload() {
        check(&[0, 1, 0], false).unwrap();
    }
    #[test]
    fn duplicate_headers_in_one_layer_are_not_hidden_by_indexing() {
        assert!(check(&[2], false).is_err());
    }
    #[test]
    fn duplicate_payloads_across_layers_are_not_hidden_by_overlay_lookup() {
        assert!(check(&[1, 1], false).is_err());
    }
    #[test]
    fn missing_or_corrupted_payload_fails() {
        assert!(check(&[0], false).is_err());
        assert!(check(&[1], true).is_err());
    }
}
