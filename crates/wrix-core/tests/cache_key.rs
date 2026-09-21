use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

use wrix_core::cache_key::{CachePublicKey, ensure_keypair, generate_keypair};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const PUBLIC_KEY: &str = "wrix-cache:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

struct Fixture {
    root: tempfile::TempDir,
    secret: PathBuf,
    public: PathBuf,
    generator: PathBuf,
}

impl Fixture {
    fn new(output: &str) -> TestResult<Self> {
        let root = tempfile::tempdir()?;
        let generator = root.path().join("nix-store");
        fs::write(generator.with_extension("pub"), output)?;
        fs::write(
            &generator,
            r#"#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == --generate-binary-cache-key ]]
[[ "$2" == wrix-cache ]]
printf 'generated-secret\n' >"$3"
cp "$0.pub" "$4"
printf 'generated\n' >>"$0.calls"
"#,
        )?;
        fs::set_permissions(&generator, fs::Permissions::from_mode(0o755))?;
        Ok(Self {
            secret: root.path().join("cache.secret"),
            public: root.path().join("cache.pub"),
            root,
            generator,
        })
    }

    fn ensure(&self) -> TestResult {
        ensure_keypair(
            "wrix-cache",
            &self.secret,
            &self.public,
            self.generator.to_str().ok_or("non-UTF-8 fixture")?,
        )?;
        Ok(())
    }
}

#[test]
fn generated_keys_are_reused_including_unpadded_public_keys() -> TestResult {
    let fixture = Fixture::new(PUBLIC_KEY)?;
    fixture.ensure()?;
    assert_eq!(
        CachePublicKey::parse(&fs::read_to_string(&fixture.public)?)?.as_str(),
        PUBLIC_KEY
    );
    for key in [PUBLIC_KEY, PUBLIC_KEY.trim_end_matches('=')] {
        fs::write(&fixture.public, format!("{key}\n"))?;
        fixture.ensure()?;
        assert_eq!(fs::read_to_string(&fixture.public)?, format!("{key}\n"));
        assert_eq!(fs::read_to_string(&fixture.secret)?, "generated-secret\n");
    }
    assert_eq!(
        fs::read_to_string(fixture.generator.with_extension("calls"))?,
        "generated\n"
    );
    Ok(())
}

#[test]
fn invalid_existing_keys_are_regenerated_through_the_shared_parser() -> TestResult {
    for invalid in [
        "wrix-cache:placeholder",
        "wrix-cache:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB=",
        "wrix-cache:!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!=",
    ] {
        let fixture = Fixture::new(PUBLIC_KEY)?;
        fs::write(&fixture.secret, "old-secret")?;
        fs::write(&fixture.public, invalid)?;
        fixture.ensure()?;
        assert_eq!(fs::read_to_string(&fixture.public)?, PUBLIC_KEY);
        assert_eq!(fs::read_to_string(&fixture.secret)?, "generated-secret\n");
    }
    Ok(())
}

#[test]
fn invalid_generator_output_preserves_old_keys_and_cleans_temporary_files() -> TestResult {
    let fixture = Fixture::new("wrix-cache:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB=")?;
    fs::write(&fixture.secret, "old-secret")?;
    fs::write(&fixture.public, PUBLIC_KEY)?;
    let error = generate_keypair(
        "wrix-cache",
        &fixture.secret,
        &fixture.public,
        fixture.generator.to_str().ok_or("non-UTF-8 fixture")?,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("generated project cache public key is invalid")
    );
    assert_eq!(fs::read_to_string(&fixture.public)?, PUBLIC_KEY);
    assert_eq!(fs::read_to_string(&fixture.secret)?, "old-secret");
    for entry in fs::read_dir(fixture.root.path())? {
        assert_ne!(
            entry?.path().extension().and_then(std::ffi::OsStr::to_str),
            Some("tmp")
        );
    }
    Ok(())
}

#[test]
fn unreadable_existing_public_key_is_not_treated_as_a_missing_key() -> TestResult {
    let fixture = Fixture::new(PUBLIC_KEY)?;
    fs::write(&fixture.secret, "old-secret")?;
    fs::create_dir(&fixture.public)?;
    assert!(fixture.ensure().is_err());
    assert_eq!(fs::read_to_string(&fixture.secret)?, "old-secret");
    assert!(!fixture.generator.with_extension("calls").exists());
    Ok(())
}
