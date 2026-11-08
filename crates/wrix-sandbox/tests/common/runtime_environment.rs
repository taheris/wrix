//! Independent external-runtime fixture reader, not the production decoder.
use std::{collections::BTreeMap, error::Error, fs, path::Path};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[allow(
    dead_code,
    reason = "Standalone lifecycle targets assert ordered record multiplicity; environment targets also use the mapped view."
)]
pub fn read(path: &Path) -> Result<BTreeMap<String, String>> {
    Ok(read_pairs(path)?.into_iter().collect())
}

pub fn read_pairs(path: &Path) -> Result<Vec<(String, String)>> {
    let bytes = fs::read(path)?;
    let bytes = bytes
        .strip_prefix(b"wrix-runtime-env-v1\0")
        .ok_or("invalid captured environment version")?;
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let fields = bytes
        .strip_suffix(&[0])
        .ok_or("truncated captured environment")?
        .split(|byte| *byte == 0)
        .collect::<Vec<_>>();
    let (pairs, remainder) = fields.as_chunks::<2>();
    if !remainder.is_empty() {
        return Err("odd captured environment record".into());
    }
    pairs
        .iter()
        .map(|pair| {
            Ok((
                String::from_utf8(pair[0].to_vec())?,
                String::from_utf8(pair[1].to_vec())?,
            ))
        })
        .collect()
}
