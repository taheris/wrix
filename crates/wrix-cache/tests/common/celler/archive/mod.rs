use std::{fs, path::PathBuf, process::Command};

use attic::{
    api::v1::upload_path::UploadPathNarInfo, cache::CacheName, hash::Hash, nix_store::StorePathHash,
};

pub struct Archive {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

impl Archive {
    pub fn file(contents: &str, name: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join(name);
        fs::write(&source, contents).unwrap();
        let output = Command::new("nix-store")
            .args(["--dump"])
            .arg(source)
            .env("NIX_REMOTE", "unix:///no-test-store-daemon")
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        let digest = Hash::sha256_from_bytes(&output.stdout).to_typed_base32();
        let hash = digest.strip_prefix("sha256:").unwrap();
        Self {
            path: PathBuf::from(format!("/nix/store/{}-{name}", &hash[..32])),
            bytes: output.stdout,
        }
    }

    pub fn metadata(&self, namespace: &str) -> UploadPathNarInfo {
        UploadPathNarInfo {
            cache: CacheName::new(namespace.into()).unwrap(),
            store_path_hash: StorePathHash::new(super::hash(&self.path).into()).unwrap(),
            store_path: self.path.to_str().unwrap().into(),
            references: Vec::new(),
            system: None,
            deriver: None,
            sigs: Vec::new(),
            ca: None,
            nar_hash: Hash::sha256_from_bytes(&self.bytes),
            nar_size: self.bytes.len(),
        }
    }
}
