//! Native absolute path descriptions, never filesystem admission.
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Component, Path, PathBuf},
};

pub(super) fn serialize<S: serde::Serializer>(
    path: &Path,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    path.as_os_str().as_bytes().serialize(serializer)
}
pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<PathBuf, D::Error> {
    let bytes = Vec::<u8>::deserialize(deserializer)?;
    let path = PathBuf::from(OsString::from_vec(bytes.clone()));
    if bytes.len() > 4096
        || bytes.contains(&0)
        || !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        || path.components().collect::<PathBuf>().as_os_str() != path.as_os_str()
    {
        return Err(serde::de::Error::custom("invalid native projection path"));
    }
    Ok(path)
}

#[cfg(test)]
mod test {
    use super::*;
    #[derive(Serialize, Deserialize)]
    struct Native {
        #[serde(with = "super")]
        path: PathBuf,
    }
    #[test]
    fn projection_paths_preserve_native_bytes_without_accepting_relative_or_routing_inputs() {
        let path = PathBuf::from(OsString::from_vec(b"/state/a space/\xff".to_vec()));
        let bytes = serde_json::to_vec(&Native { path: path.clone() }).unwrap();
        assert_eq!(serde_json::from_slice::<Native>(&bytes).unwrap().path, path);
        for value in [
            b"../scope".to_vec(),
            b"/state/../peer".to_vec(),
            b"/state\0private".to_vec(),
        ] {
            assert!(serde_json::from_value::<Native>(serde_json::json!({"path":value})).is_err());
        }
    }
}
