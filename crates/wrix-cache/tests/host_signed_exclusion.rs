//! Adversarial signed-cache controls use only disposable rooted stores and test caches.
#![cfg(test)]

use std::{
    fs,
    path::Path,
    process::{Command, Output},
    time::Duration,
};
use wrix_cache::backend::issuer::Authority;
use wrix_core::path::WorkspaceHash;

#[path = "common/host_policy/mod.rs"]
mod bootstrap;
// Reuse the production-Celler fixture; its isolated-daemon helpers are unrelated here.
#[allow(dead_code)]
#[path = "common/celler/mod.rs"]
mod native;

fn realise_at(configuration: &str, path: &Path, root: &Path) -> Output {
    Command::new("nix-store")
        .args([
            "--store",
            &format!("local?root={}", root.display()),
            "--realise",
        ])
        .arg(path)
        .env("HOME", root)
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("NIX_CONF_DIR", root.join("no-system-config"))
        .env("NIX_USER_CONF_FILES", "/dev/null")
        .env("NIX_CONFIG", configuration)
        .env("NIX_REMOTE", "unix:///no-test-store-daemon")
        .output()
        .unwrap()
}

fn realise(configuration: &str, path: &Path) -> (Output, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    (realise_at(configuration, path, root.path()), root)
}

#[tokio::test]
async fn signed_malicious_artifact_reaches_the_sandbox_but_not_the_production_host_configuration() {
    let server = native::Server::start_with_issuer();
    let authority = Authority::open(&server.issuer_root()).unwrap();
    let grant = authority
        .pull_credential(&WorkspaceHash::parse(&"a".repeat(64)).unwrap())
        .unwrap();
    let namespace = grant.views().private().as_str();
    let key = server
        .client(namespace)
        .register(Duration::from_hours(720))
        .await
        .unwrap();
    let malicious =
        native::archive::Archive::file("malicious signed replacement", "malicious-replacement");
    server.upload(namespace, &malicious).await;
    let sandbox = native::import::Store::new();
    let imported = sandbox.copy(
        &format!("{}{namespace}", server.endpoint),
        grant.expose_bearer(),
        key.as_str(),
        &malicious.path,
    );
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    assert_eq!(
        sandbox.read(&malicious.path).unwrap(),
        b"malicious signed replacement"
    );

    let mut fixture = bootstrap::Fixture::new();
    fixture.cache = fixture.root.path().join("adversarial-cache");
    fs::create_dir(&fixture.cache).unwrap();
    fixture.key = key.as_str().to_owned();
    fs::write(fixture.state.join("keys/cache.pub"), &fixture.key).unwrap();
    fs::write(
        fixture.root.path().join("endpoints.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "workspace_hash": "a".repeat(64), "state_root": fixture.state,
            "cache_root": fixture.cache, "endpoints": {"cache_http": {
                "host": "127.0.0.1", "port": reqwest::Url::parse(&server.endpoint).unwrap().port().unwrap(),
            }},
        }))
        .unwrap(),
    )
    .unwrap();

    // This file cache is an adversarial test mirror, never a production fallback.
    // The actual Celler signature authenticates the same content via either transport.
    let info = reqwest::Client::new()
        .get(format!(
            "{}{namespace}/{}.narinfo",
            server.endpoint,
            native::hash(&malicious.path)
        ))
        .bearer_auth(grant.expose_bearer())
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .await
        .unwrap();
    let info = info
        .lines()
        .map(|line| {
            if line.starts_with("URL: ") {
                "URL: payload.nar"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        fixture
            .cache
            .join(format!("{}.narinfo", native::hash(&malicious.path))),
        format!("{info}\n"),
    )
    .unwrap();
    fs::write(fixture.cache.join("payload.nar"), &malicious.bytes).unwrap();
    fs::write(
        fixture.cache.join("nix-cache-info"),
        "StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 40\n",
    )
    .unwrap();

    let baseline = "experimental-features = nix-command\nrequire-sigs = true\nsubstituters =\ntrusted-public-keys =";
    let activation = fixture.run(baseline);
    assert!(
        activation.status.success(),
        "{}",
        String::from_utf8_lossy(&activation.stderr)
    );
    let production = fs::read_to_string(fixture.root.path().join("host-config")).unwrap();
    let (denied, host) = realise(&production, &malicious.path);
    assert!(!denied.status.success());
    assert!(
        !host
            .path()
            .join("nix/store")
            .join(malicious.path.file_name().unwrap())
            .exists()
    );

    // A reachable valid mirror and unchanged key must work when deliberately trusted.
    // Otherwise the negative host result could merely be a broken fixture.
    let unsafe_configuration = format!(
        "{baseline}\nextra-substituters = {}\nextra-trusted-public-keys = {}",
        reqwest::Url::from_file_path(&fixture.cache).unwrap(),
        fixture.key
    );
    let (accepted, unsafe_host) = realise(&unsafe_configuration, &malicious.path);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    // Exclusion does not delete, recall or attest outputs already imported under other trust.
    let reused = realise_at(&production, &malicious.path, unsafe_host.path());
    assert!(
        reused.status.success(),
        "{}",
        String::from_utf8_lossy(&reused.stderr)
    );
    assert_eq!(
        fs::read(
            unsafe_host
                .path()
                .join("nix/store")
                .join(malicious.path.file_name().unwrap())
        )
        .unwrap(),
        b"malicious signed replacement"
    );
}
