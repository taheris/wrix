use std::fs;

use serde_json::{Value, json};
use wrix_sandbox::command::config::{Platform, load_profile_config, load_spawn_config};

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

fn profile_value(platform: Platform) -> Value {
    json!({
        "schema": 1,
        "system": if platform == Platform::Linux { "x86_64-linux" } else { "aarch64-darwin" },
        "profile": {
            "name": "example", "env": {}, "mounts": [{"source": "/host", "dest": "/data"}],
            "network_allowlist": [{"host": "example.org", "ports": [443]}]
        },
        "image": {"ref": "wrix:test", "source": "/fixture", "source_kind": platform.expected_source_kind().as_str()},
        "agent": {"kind": "direct"},
        "resources": {"cpus": null, "memory_mb": 4096, "pids_limit": 4096},
        "security": {"deploy_key": null, "runtime_secrets": {}},
        "network": {"default_mode": "open", "ipv6": "disabled"},
        "services": {"beads": {"enable": "auto"}, "nix_cache": {"enable": true}},
        "features": {"mcp_runtime": false}
    })
}

#[test]
fn shared_image_metadata_is_optional_and_independent_of_private_profiles() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("profile.json");
    let seed = "/nix/store/00000000000000000000000000000000-closure-info/store-paths";
    for platform in [Platform::Linux, Platform::Darwin] {
        let mut value = profile_value(platform);
        fs::write(&path, value.to_string()).unwrap();
        assert!(
            load_profile_config(&path, platform)
                .unwrap()
                .image
                .shared_nix
                .is_none()
        );
        for version in [1, 2] {
            value["image"]["shared_nix"] = json!({"version": version, "seed_closure": seed});
            fs::write(&path, value.to_string()).unwrap();
            let image = load_profile_config(&path, platform).unwrap().image;
            let metadata = image.shared_nix.unwrap();
            assert_eq!(metadata.version.require_supported().is_ok(), version == 1);
            assert_eq!(metadata.seed_closure.as_path().to_str(), Some(seed));
        }
        for invalid in [
            json!({}),
            json!({"version": 0, "seed_closure": seed}),
            json!({"version": 1, "seed_closure": "/tmp/store-paths"}),
            json!({"version": 1, "seed_closure": seed, "approve": true}),
        ] {
            value["image"]["shared_nix"] = invalid;
            fs::write(&path, value.to_string()).unwrap();
            assert!(load_profile_config(&path, platform).is_err());
        }
    }
}

#[test]
fn managed_cache_declarations_are_closed_and_platform_independent() {
    use wrix_sandbox::command::config::ManagedCache;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("profile.json");
    for platform in [Platform::Linux, Platform::Darwin] {
        let mut value = profile_value(platform);
        value["profile"]["tool_caches"] = json!(["cargo", "sccache", "uv"]);
        fs::write(&path, value.to_string()).unwrap();
        assert_eq!(
            load_profile_config(&path, platform)
                .unwrap()
                .profile
                .tool_caches,
            [ManagedCache::Cargo, ManagedCache::Sccache, ManagedCache::Uv]
        );
        for invalid in [json!(["personal"]), json!([{}]), json!({}), json!(null)] {
            value["profile"]["tool_caches"] = invalid;
            fs::write(&path, value.to_string()).unwrap();
            assert!(load_profile_config(&path, platform).is_err());
        }
    }
}

fn spawn_value() -> Value {
    json!({
        "workspace": "/repo", "env": [], "agent_args": [],
        "git": {}, "tool_caches": {},
        "mounts": [{"host_path": "/host", "container_path": "/data", "read_only": true}]
    })
}

#[test]
fn profile_owned_structures_reject_unknown_fields() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("profile.json");
    for pointer in [
        "",
        "/profile",
        "/profile/mounts/0",
        "/profile/network_allowlist/0",
        "/image",
        "/agent",
        "/resources",
        "/security",
        "/network",
        "/services",
        "/services/beads",
        "/services/nix_cache",
        "/features",
    ] {
        let mut value = profile_value(Platform::Linux);
        value.pointer_mut(pointer).unwrap()["unknown"] = json!(true);
        fs::write(&path, value.to_string()).unwrap();
        assert!(
            load_profile_config(&path, Platform::Linux).is_err(),
            "accepted {pointer}"
        );
    }
    for (pointer, invalid) in [
        ("/system", json!(7)),
        ("/system", json!("unsupported")),
        ("/features/mcp_runtime", json!("yes")),
        ("/services/beads/enable", json!(true)),
        ("/services/beads/enable", json!("always")),
        ("/services/nix_cache/enable", json!("yes")),
    ] {
        let mut value = profile_value(Platform::Linux);
        *value.pointer_mut(pointer).unwrap() = invalid;
        fs::write(&path, value.to_string()).unwrap();
        assert!(
            load_profile_config(&path, Platform::Linux).is_err(),
            "accepted {pointer}"
        );
    }
}

#[test]
fn spawn_owned_structures_reject_unknown_fields() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("spawn.json");
    for pointer in ["", "/git", "/tool_caches", "/mounts/0"] {
        let mut value = spawn_value();
        value.pointer_mut(pointer).unwrap()["unknown"] = json!(true);
        fs::write(&path, value.to_string()).unwrap();
        assert!(
            load_spawn_config(&path, Platform::Linux).is_err(),
            "accepted {pointer}"
        );
    }
    for field in [
        "initial_prompt",
        "repin",
        "agent",
        "profile",
        "image_digest_path",
    ] {
        let mut value = spawn_value();
        value[field] = json!("consumer data");
        fs::write(&path, value.to_string()).unwrap();
        assert!(
            load_spawn_config(&path, Platform::Linux).is_err(),
            "accepted {field}"
        );
    }
}

#[test]
fn explicit_extensions_are_optional_opaque_objects() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("profile.json");
    let spawn = root.path().join("spawn.json");
    for extensions in [
        None,
        Some(json!({})),
        Some(json!({"nested": [null, true, {"arbitrary": 1}]})),
    ] {
        let mut p = profile_value(Platform::Linux);
        let mut s = spawn_value();
        if let Some(extensions) = extensions {
            p["extensions"] = extensions.clone();
            s["extensions"] = extensions;
        }
        fs::write(&profile, p.to_string()).unwrap();
        fs::write(&spawn, s.to_string()).unwrap();
        assert!(load_profile_config(&profile, Platform::Linux).is_ok());
        assert!(load_spawn_config(&spawn, Platform::Linux).is_ok());
    }
    for extensions in [
        json!(null),
        json!(false),
        json!(1),
        json!("data"),
        json!([]),
    ] {
        let mut p = profile_value(Platform::Linux);
        let mut s = spawn_value();
        p["extensions"] = extensions.clone();
        s["extensions"] = extensions;
        fs::write(&profile, p.to_string()).unwrap();
        fs::write(&spawn, s.to_string()).unwrap();
        assert!(load_profile_config(&profile, Platform::Linux).is_err());
        assert!(load_spawn_config(&spawn, Platform::Linux).is_err());
    }
}

#[test]
fn explicit_profile_extensions_preserve_source_semantics() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("profile.json");
    for platform in [Platform::Linux, Platform::Darwin] {
        let mut value = profile_value(platform);
        value["extensions"] = json!({"image": {"source_kind": "invented"}});
        fs::write(&path, value.to_string()).unwrap();
        let config = load_profile_config(&path, platform).unwrap();
        assert_eq!(config.image.source.kind(), platform.expected_source_kind());
        assert_eq!(config.image.source.path(), std::path::Path::new("/fixture"));
        let other = if platform == Platform::Linux {
            Platform::Darwin
        } else {
            Platform::Linux
        };
        assert!(load_profile_config(&path, other).is_err());
    }
}

#[test]
fn extension_payloads_do_not_override_launch_policy() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("profile.json");
    let spawn = root.path().join("spawn.json");
    let payload = json!({
        "agent": {"kind": "pi"}, "image": {"ref": "evil:test", "source": "/other"},
        "profile": {"env": {"EXTRA": "value"}, "mounts": []},
        "network": {"default_mode": "limit"}, "resources": {"memory_mb": 1},
        "git": {"deploy": true, "sign": true}, "tool_caches": {"shared": false},
        "workspace": "/other", "env": [["EXTRA", "value"]], "agent_args": ["changed"],
        "mounts": [], "security": {"runtime_secrets": {"TOKEN": "required"}},
        "services": {"nix_cache": {"enable": false}}, "features": {"mcp_runtime": true}
    });
    let mut p = profile_value(Platform::Linux);
    let mut s = spawn_value();
    p["extensions"] = payload.clone();
    s["extensions"] = payload;
    fs::write(&profile, p.to_string()).unwrap();
    fs::write(&spawn, s.to_string()).unwrap();
    let p = load_profile_config(&profile, Platform::Linux).unwrap();
    let s = load_spawn_config(&spawn, Platform::Linux).unwrap();
    assert_eq!(p.agent.kind.as_str(), "direct");
    assert_eq!(p.image.reference.as_str(), "wrix:test");
    assert_eq!(p.image.source.path(), std::path::Path::new("/fixture"));
    assert!(p.profile.env.is_empty());
    assert_eq!(p.profile.mounts.len(), 1);
    assert_eq!(p.network.default_mode.as_str(), "open");
    assert_eq!(p.resources.memory_mb, 4096);
    assert!(p.security.runtime_secrets.is_empty());
    assert!(p.services.nix_cache.enabled);
    assert_eq!(s.workspace, "/repo");
    assert!(s.image_ref.is_none());
    assert!(s.image_source.is_none());
    assert_eq!(s.env, []);
    assert_eq!(s.agent_args, [] as [String; 0]);
    assert_eq!(s.mounts.len(), 1);
    assert_eq!(s.git.deploy, None);
    assert_eq!(s.git.sign, None);
    assert_eq!(s.tool_caches, wrix_core::tool_cache::Preference::default());
}

#[test]
fn strict_boundaries_preserve_existing_map_and_default_semantics() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("profile.json");
    let spawn = root.path().join("spawn.json");
    let p = json!({
        "schema": 1, "profile": {"name": "base", "env": {"CUSTOM_DATA_1": "value"}},
        "image": {"ref": "wrix:test", "source": "/fixture", "source_kind": "nix-descriptor"},
        "agent": {"kind": "direct"}, "security": {"runtime_secrets": {"CUSTOM_TOKEN_1": "required"}}
    });
    fs::write(&profile, p.to_string()).unwrap();
    let parsed = load_profile_config(&profile, Platform::Linux).unwrap();
    assert_eq!(
        parsed.profile.env.keys().next().unwrap().as_str(),
        "CUSTOM_DATA_1"
    );
    assert_eq!(
        parsed
            .security
            .runtime_secrets
            .keys()
            .next()
            .unwrap()
            .as_str(),
        "CUSTOM_TOKEN_1"
    );
    assert_eq!(parsed.resources.memory_mb, 4096);
    assert!(parsed.services.nix_cache.enabled);
    assert!(parsed.profile.mounts.is_empty());
    assert_eq!(parsed.profile.network_allowlist, []);
    for field in ["env", "runtime_secrets"] {
        let mut invalid = p.clone();
        if field == "env" {
            invalid["profile"][field] = json!({"not valid": "value"});
        } else {
            invalid["security"][field] = json!({"not valid": "optional"});
        }
        fs::write(&profile, invalid.to_string()).unwrap();
        assert!(load_profile_config(&profile, Platform::Linux).is_err());
    }
    for duplicate in [
        r#""schema":1,"schema":1"#,
        r#""features":{"mcp_runtime":true,"mcp_runtime":false},"schema":1"#,
        r#""services":{"beads":{"enable":"auto","enable":"auto"}},"schema":1"#,
        r#""extensions":{},"extensions":{},"schema":1"#,
    ] {
        let value = p.to_string().replacen("\"schema\":1", duplicate, 1);
        fs::write(&profile, value).unwrap();
        assert!(load_profile_config(&profile, Platform::Linux).is_err());
    }
    fs::write(&spawn, r#"{"workspace":"/repo","env":[],"agent_args":[]}"#).unwrap();
    let parsed = load_spawn_config(&spawn, Platform::Linux).unwrap();
    assert!(parsed.mounts.is_empty());
    assert_eq!(parsed.git.deploy, None);
    for duplicate in [
        r#""workspace":"/repo","workspace":"/other""#,
        r#""workspace":"/repo","git":{"deploy":true,"deploy":false}"#,
        r#""workspace":"/repo","extensions":{},"extensions":{}"#,
    ] {
        fs::write(
            &spawn,
            format!("{{{duplicate},\"env\":[],\"agent_args\":[]}}"),
        )
        .unwrap();
        assert!(load_spawn_config(&spawn, Platform::Linux).is_err());
    }
}

#[test]
fn documented_spawn_example_matches_parser() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("spawn.json");
    fs::write(
        &path,
        r#"{
        "workspace": "/repo", "env": [], "agent_args": [], "mounts": [],
        "extensions": {"initial_prompt": "consumer field", "repin": true}
    }"#,
    )
    .unwrap();
    let config = load_spawn_config(&path, Platform::Linux).unwrap();
    assert_eq!(config.workspace, "/repo");
    assert!(config.mounts.is_empty());
}

#[test]
#[ignore = "requires real Nix-generated built-in ProfileConfig files"]
fn nix_generated_builtin_profiles_match_parser() {
    let manifest = std::env::var_os("WRIX_TEST_PROFILE_CONFIGS").unwrap();
    let cases: Vec<Value> = serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
    assert_eq!(cases.len(), 6);
    for case in cases {
        let path = std::path::Path::new(case["path"].as_str().unwrap());
        let platform = match case["system"].as_str().unwrap() {
            "x86_64-linux" => Platform::Linux,
            "aarch64-darwin" => Platform::Darwin,
            other => panic!("unexpected fixture system {other}"),
        };
        let config = load_profile_config(path, platform).unwrap();
        assert_eq!(
            config.profile.name.to_string(),
            case["name"].as_str().unwrap()
        );
        assert_eq!(config.image.source.kind(), platform.expected_source_kind());
        assert!(config.image.digest.is_some());
        let shared = config.image.shared_nix.as_ref().unwrap();
        shared.version.require_supported().unwrap();
        assert!(shared.seed_closure.as_path().is_file());
        assert_eq!(config.agent.kind.as_str(), "pi");
        let wire: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(wire["schema"], 1);
        assert_eq!(wire["system"], case["system"]);
        assert_eq!(wire["services"]["beads"]["enable"], "auto");
        assert_eq!(wire["features"]["mcp_runtime"], false);
        assert_eq!(wire["resources"]["memory_mb"], config.resources.memory_mb);
        assert_eq!(wire["image"]["ref"], config.image.reference.as_str());
    }
}
