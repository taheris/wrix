use super::*;

#[test]
fn generated_oci_configurations_keep_the_compact_token_digest() {
    for seed in 0..512_u32 {
        let config = serde_json::json!({
            "created": "2026-01-01T00:00:00Z",
            "architecture": "arm64",
            "os": "linux",
            "config": {
                "Env": [format!("SEED={seed}"), "QUOTED=\\\" λ", "SPACES=two  words", "HTML=<>&"],
                "Labels": { "seed": seed.to_string(), "enabled": (seed % 2 == 0).to_string() },
                "User": "1000:1000",
            },
            "rootfs": { "type": "layers", "diff_ids": [format!("sha256:{seed:064x}")] },
        });
        let compact = serde_json::to_vec(&config).unwrap();
        let pretty = serde_json::to_vec_pretty(&config).unwrap();
        assert_eq!(
            config_digest(&pretty).unwrap(),
            raw_hash(&compact),
            "seed {seed}"
        );
    }
}

#[test]
fn duplicate_object_keys_are_not_rewritten_by_validation() {
    assert_eq!(
        config_digest(br#"{ "key": 1, "key": 2 }"#).unwrap(),
        raw_hash(br#"{"key":1,"key":2}"#)
    );
}
