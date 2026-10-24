//! Describes one immutable selected image; native inspection and checked scope still supply authority.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest(String);
#[derive(Debug, displaydoc::Display, thiserror::Error)]
/// selected image must have a complete sha256 configuration digest
pub struct Error;
impl TryFrom<String> for Digest {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Error> {
        let hash = value.strip_prefix("sha256:").ok_or(Error)?;
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error);
        }
        Ok(Self(format!("sha256:{}", hash.to_ascii_lowercase())))
    }
}
impl From<Digest> for String {
    fn from(value: Digest) -> Self {
        value.0
    }
}
impl Digest {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    digest: Digest,
    metadata: super::Image,
}
impl Binding {
    pub fn new(digest: &str, metadata: super::Image) -> Result<Self, Error> {
        Ok(Self {
            digest: Digest::try_from(digest.to_owned())?,
            metadata,
        })
    }
    pub const fn digest(&self) -> &Digest {
        &self.digest
    }
    pub const fn metadata(&self) -> &super::Image {
        &self.metadata
    }
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn immutable_selection_rejects_tags_paths_missing_digest_and_unknown_owned_fields() {
        for value in [
            "latest",
            "../image",
            "sha256:short",
            &format!("sha256:{}", "g".repeat(64)),
        ] {
            assert!(Digest::try_from(value.to_owned()).is_err());
        }
        let json = serde_json::json!({"digest":format!("sha256:{}", "a".repeat(64)),"metadata":{"version":1,"seed_closure":"/nix/store/00000000000000000000000000000000-closure-info/store-paths"}});
        assert!(serde_json::from_value::<Binding>(json.clone()).is_ok());
        let mut extra = json;
        extra["command"] = serde_json::json!("/bin/sh");
        assert!(serde_json::from_value::<Binding>(extra).is_err());
    }
}
