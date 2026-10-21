#![cfg(test)]

use base64::{Engine, engine::general_purpose::STANDARD};
use std::{fs, os::unix::fs::MetadataExt, process::Command};
#[path = "common/host_policy/mod.rs"]
mod bootstrap;
use bootstrap::Fixture;

#[test]
fn host_configuration_is_publication_only_and_preserves_operator_settings() {
    let fixture = Fixture::new();
    let credentials = fixture.root.path().join("operator-netrc");
    fs::write(&credentials, "independent operator credentials").unwrap();
    let credential_inode = fs::metadata(&credentials).unwrap().ino();
    let mut configuration = format!(
        "experimental-features = nix-command\nsubstituters = https://cache.nixos.org\ntrusted-public-keys = upstream:{}\nbuilders-use-substitutes = false\nnarinfo-cache-negative-ttl = 777\nnetrc-file = {}",
        STANDARD.encode([1; 32]),
        credentials.display(),
    );
    for _ in 0..2 {
        let output = fixture.run(&configuration);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(!text.contains(&fixture.key));
        assert!(!text.contains("extra-substituters"));
        assert!(!text.contains("extra-trusted-public-keys"));
        assert!(text.contains("builders-use-substitutes = false"));
        assert!(text.contains("narinfo-cache-negative-ttl = 777"));
        assert!(text.contains("substituters = https://cache.nixos.org"));
        assert!(text.contains("post-build-hook = "));
        let status = Command::new("bash")
            .arg(
                fixture
                    .root
                    .path()
                    .join("hook-store/bin/wrix-cache-post-build-hook"),
            )
            .current_dir(fixture.root.path())
            .env("FIXTURE", fixture.root.path())
            .status()
            .unwrap();
        assert!(status.success());
        let bytes = fs::read(fixture.root.path().join("hook-arguments")).unwrap();
        let arguments: Vec<&[u8]> = bytes.split(|byte| *byte == 0).collect();
        assert!(arguments.contains(&fixture.state.as_os_str().as_encoded_bytes()));
        assert!(arguments.contains(&fixture.cache.as_os_str().as_encoded_bytes()));
        assert!(!fixture.root.path().join("MUST-NOT-EXIST").exists());
        assert_eq!(fs::metadata(&credentials).unwrap().ino(), credential_inode);
        assert_eq!(
            fs::read_to_string(&credentials).unwrap(),
            "independent operator credentials"
        );
        configuration = fs::read_to_string(fixture.root.path().join("host-config")).unwrap();
        assert_eq!(
            configuration
                .lines()
                .filter(|line| line.starts_with("post-build-hook = "))
                .count(),
            1
        );
    }
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("service-calls"))
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        [
            "service cache config",
            "service endpoints",
            "service cache config",
            "service endpoints"
        ]
    );
}

#[test]
fn disabled_host_cache_is_passive_even_with_unsafe_inherited_trust_and_bad_metadata() {
    let fixture = Fixture::new();
    fs::write(
        fixture.state.join("keys/cache.pub"),
        "malformed retained metadata",
    )
    .unwrap();
    fs::write(
        fixture.root.path().join("endpoints.json"),
        r#"{"endpoints": {"cache_http": null}}"#,
    )
    .unwrap();
    let configuration = format!(
        "experimental-features = nix-command\nextra-trusted-public-keys = {}",
        fixture.key
    );
    let output = fixture.run(&configuration);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("host-config")).unwrap(),
        configuration
    );
    assert!(!fixture.state.join("hook-wrapper").exists());
    assert_eq!(
        fs::read_to_string(fixture.state.join("keys/cache.pub")).unwrap(),
        "malformed retained metadata"
    );
}

#[test]
fn inherited_shared_cache_trust_requires_remediation_before_hook_mutation() {
    let fixture = Fixture::new();
    for setting in [
        format!("trusted-public-keys = {}", fixture.key),
        "substituters = http://localhost:41000/wrix-common-v1?trusted=1".into(),
    ] {
        let original = format!("experimental-features = nix-command\n{setting}");
        let output = fixture.run(&original);
        assert!(!output.status.success());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("remove these entries from `NIX_CONFIG`/nix.conf"));
        assert!(!fixture.state.join("hook-wrapper").exists());
        assert!(!fixture.root.path().join("hook-store").exists());
        assert_eq!(
            fs::read_to_string(fixture.state.join("keys/cache.pub")).unwrap(),
            fixture.key
        );
    }
}
