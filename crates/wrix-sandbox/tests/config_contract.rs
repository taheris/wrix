use std::fs;

use serde_json::json;
use wrix_sandbox::command::config::{Platform, load_profile_config};

#[test]
fn profile_network_rule_objects_match_documented_wire() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("profile.json");
    let rules = json!([
        {"host": "github.com", "ports": [22, 443]},
        {"host": "registry.example.org", "ports": [8443]}
    ]);
    let mut value = json!({
        "schema": 1,
        "profile": {"name": "example", "network_allowlist": rules},
        "image": {"ref": "wrix:test", "source": "/fixture", "source_kind": "nix-descriptor"},
        "agent": {"kind": "direct"}
    });
    fs::write(&path, value.to_string()).unwrap();
    let config = load_profile_config(&path, Platform::Linux).unwrap();
    assert_eq!(
        serde_json::to_value(&config.profile.network_allowlist).unwrap(),
        rules
    );
    assert_eq!(config.profile.network_allowlist[0].host(), "github.com");
    assert_eq!(config.profile.network_allowlist[1].ports()[0].get(), 8443);

    value["profile"]["network_allowlist"][0]["extra"] = json!(true);
    fs::write(&path, value.to_string()).unwrap();
    assert!(load_profile_config(&path, Platform::Linux).is_err());
}
