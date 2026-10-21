use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};

struct Fixture {
    root: tempfile::TempDir,
    state: PathBuf,
    cache: PathBuf,
    key: String,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("workspaces").join("a".repeat(64));
        let cache = root.path().join("cache with spaces");
        fs::create_dir_all(state.join("keys")).unwrap();
        fs::create_dir(&cache).unwrap();
        let key = format!("retained-custom-name:{}", STANDARD.encode([0; 32]));
        fs::write(state.join("keys/cache.pub"), &key).unwrap();
        Self {
            root,
            state,
            cache,
            key,
        }
    }

    fn inventory(&self) -> Inventory {
        Inventory::read(&self.state, &self.cache, "127.0.0.1:41000").unwrap()
    }
}

#[test]
fn endpoint_metadata_requires_the_native_shape_and_explicit_disabled_state() {
    assert!(
        cache_endpoint(r#"{"endpoints":{"cache_http":null}}"#)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        cache_endpoint(r#"{"endpoints":{"cache_http":{"host":"127.0.0.1","port":41000}}}"#)
            .unwrap()
            .unwrap()
            .port(),
        41000
    );
    for value in [
        r#"{"cache_http":null}"#,
        r#"{"endpoints":{}}"#,
        r#"{"endpoints":{"cache_http":"127.0.0.1:41000"}}"#,
        r#"{"endpoints":{"cache_http":{"host":"0.0.0.0","port":41000}}}"#,
        r#"{"endpoints":{"cache_http":{"host":"127.0.0.1","port":0}}}"#,
    ] {
        assert!(cache_endpoint(value).is_err());
    }
}

#[test]
fn independent_upstreams_and_operator_settings_remain_unchanged() {
    let fixture = Fixture::new();
    let text = format!(
        "substituters = https://cache.nixos.org http://localhost:41001\ntrusted-public-keys = upstream:{}\nbuilders-use-substitutes = true\nnarinfo-cache-negative-ttl = 3600\nnetrc-file = /private/operator-credentials",
        STANDARD.encode([1; 32])
    );
    let before = fs::metadata(fixture.state.join("keys/cache.pub")).unwrap();
    fixture.inventory().check(&text).unwrap();
    let after = fs::metadata(fixture.state.join("keys/cache.pub")).unwrap();
    assert_eq!(before.ino(), after.ino());
    assert_eq!(
        fs::read_to_string(fixture.state.join("keys/cache.pub")).unwrap(),
        fixture.key
    );
}

#[test]
fn inherited_service_urls_and_known_loopback_aliases_are_rejected_without_echoing_credentials() {
    let fixture = Fixture::new();
    for endpoint in [
        "http://127.0.0.1:41000/wrix-common-v1",
        "http://localhost:41000/private?trusted=1",
        "http://127.0.0.2:41000/peer",
        "http://[::1]:41000/private",
        "http://localhost.localdomain:41000/private",
        "http://0x7f000001:41000/private",
        "https://user:secret-password@127.0.0.1:41000/private",
    ] {
        for setting in [
            "substituters",
            "trusted-substituters",
            "extra-substituters",
            "extra-trusted-substituters",
        ] {
            let error = fixture
                .inventory()
                .check(&format!("{setting} = {endpoint}"))
                .unwrap_err();
            assert!(matches!(error, Error::Inherited(_)));
            assert!(!error.to_string().contains("secret-password"));
        }
    }
}

#[test]
fn file_cache_percent_encoding_symlinks_and_trust_overrides_do_not_bypass_exclusion() {
    let fixture = Fixture::new();
    let alias = fixture.root.path().join("cache-alias");
    std::os::unix::fs::symlink(&fixture.cache, &alias).unwrap();
    for path in [&fixture.cache, &alias] {
        let url = Url::from_file_path(path).unwrap();
        assert!(matches!(
            fixture
                .inventory()
                .check(&format!("substituters = {url}?trusted=1")),
            Err(Error::Inherited(_))
        ));
    }
    let unrelated = fixture.root.path().join("cache with spaces-unrelated");
    let url = Url::from_file_path(unrelated).unwrap();
    fixture
        .inventory()
        .check(&format!("substituters = {url}"))
        .unwrap();
}

#[test]
fn retained_peer_common_and_renamed_signing_material_cannot_be_trusted_by_the_host() {
    let fixture = Fixture::new();
    let peer = fixture
        .state
        .parent()
        .unwrap()
        .join("b".repeat(64))
        .join("keys");
    fs::create_dir_all(&peer).unwrap();
    let peer_key = format!("custom-peer:{}", STANDARD.encode([2; 32]));
    fs::write(peer.join("cache.pub"), &peer_key).unwrap();
    let inventory = fixture.inventory();
    for key in [
        fixture.key.clone(),
        peer_key,
        format!("renamed:{}", material(&fixture.key)),
        format!("wrix-common-v1:{}", STANDARD.encode([3; 32])),
        format!("wrix-{}:{}", "a".repeat(43), STANDARD.encode([4; 32])),
    ] {
        assert!(matches!(
            inventory.check(&format!("trusted-public-keys = {key}")),
            Err(Error::Inherited(_))
        ));
    }
}

#[test]
fn malformed_or_unsafe_retained_metadata_is_not_empty_and_is_never_repaired() {
    let fixture = Fixture::new();
    let path = fixture.state.join("keys/cache.pub");
    fs::write(&path, "malformed").unwrap();
    assert!(matches!(
        Inventory::read(&fixture.state, &fixture.cache, "127.0.0.1:41000"),
        Err(Error::Metadata)
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), "malformed");
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink("unrelated", &path).unwrap();
    assert!(Inventory::read(&fixture.state, &fixture.cache, "127.0.0.1:41000").is_err());
    assert!(
        fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
