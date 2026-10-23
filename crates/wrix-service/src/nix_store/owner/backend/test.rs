use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::json;
use std::os::unix::fs::{PermissionsExt, symlink};

fn key() -> String {
    let mut wire = b"\0\0\0\x0bssh-ed25519\0\0\0\x20".to_vec();
    wire.extend([7_u8; 32]);
    format!("ssh-ed25519 {}", STANDARD.encode(wire))
}

fn target() -> serde_json::Value {
    json!({
        "id": "a".repeat(64),
        "attempt": {
            "nonce": "b".repeat(32),
            "software": {
                "candidate": {"schema":1,"image":"localhost/pool:test","source_kind":"nix-descriptor","source":{"path":"/source","derivation":"/source.drv"},"digest":{"path":"/digest","derivation":"/digest.drv"}},
                "digest": format!("sha256:{}", "c".repeat(64)),
                "image": "c".repeat(64),
                "tools": {"version":1,"root":"/nix/store/00000000000000000000000000000000-runtime","system":"x86_64-linux"}
            }
        }
    })
}

#[test]
fn release_checkpoint_requires_private_owned_regular_evidence_and_no_resume_marker() {
    let directory = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let root = directory.path().canonicalize().unwrap();
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join("released"))
        .unwrap();
    file.write_all(b"1\n").unwrap();
    assert!(released(&root).is_ok());
    symlink("missing", root.join("resuming")).unwrap();
    assert!(matches!(released(&root), Err(Error::Retained)));
    fs::remove_file(root.join("resuming")).unwrap();
    fs::set_permissions(root.join("released"), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(released(&root).is_err());
}

#[test]
fn pinned_known_hosts_argument_preserves_spaces_quotes_and_native_bytes() {
    let path = PathBuf::from(OsString::from_vec(
        b"/state/a space/\"quote\\native\xff".to_vec(),
    ));
    assert_eq!(
        known_hosts_option(&path).unwrap().as_bytes(),
        b"UserKnownHostsFile=\"/state/a space/\\\"quote\\\\native\xff\""
    );
    assert!(known_hosts_option(Path::new("/state/new\nline")).is_err());
}

#[test]
fn configuration_digest_is_not_confused_with_a_manifest_digest() {
    let digest = Digest::parse(&format!("sha256:{}", "a".repeat(64))).unwrap();
    assert!(same_image(
        &ImageId::parse(&"a".repeat(64)).unwrap(),
        &digest
    ));
    assert!(same_image(
        &ImageId::parse(digest.as_str()).unwrap(),
        &digest
    ));
    assert!(!same_image(
        &ImageId::parse(&"b".repeat(64)).unwrap(),
        &digest
    ));
}

#[test]
fn native_runtime_identity_cannot_select_an_option_name_or_path() {
    assert!(NativeId::try_from("a".repeat(64)).is_ok());
    for value in [
        "--all".to_owned(),
        "../peer".to_owned(),
        "A".repeat(64),
        format!("{}\n", "a".repeat(64)),
    ] {
        assert!(NativeId::try_from(value).is_err());
    }
}

#[test]
fn early_intent_cannot_carry_a_ready_endpoint_or_arbitrary_container_name() {
    let attempt = target()["attempt"].clone();
    let parsed: Attempt = serde_json::from_value(attempt.clone()).unwrap();
    assert_eq!(parsed.name(), format!("wrix-nix-{}", "b".repeat(32)));
    let mut injected = attempt.clone();
    injected["name"] = json!("another-service");
    assert!(serde_json::from_value::<Attempt>(injected).is_err());
    let mut state = json!({"phase":"starting","attempt":attempt});
    state["endpoint"] = json!("127.0.0.1:22");
    assert!(serde_json::from_value::<State>(state).is_err());
}

#[test]
fn recorded_management_connection_requires_nonzero_port_and_canonical_host_key() {
    let connection = json!({"target":target(),"port":2222,"host_key":key()});
    assert!(serde_json::from_value::<Connection>(connection.clone()).is_ok());
    for port in [0, 65536] {
        let mut invalid = connection.clone();
        invalid["port"] = json!(port);
        assert!(serde_json::from_value::<Connection>(invalid).is_err());
    }
    let mut invalid = connection;
    invalid["host_key"] = json!(format!("{} comment", key()));
    assert!(serde_json::from_value::<Connection>(invalid).is_err());
}
