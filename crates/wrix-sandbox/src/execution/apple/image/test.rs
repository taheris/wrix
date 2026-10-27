use super::*;
fn fixture() -> (tempfile::NamedTempFile, Value) {
    use std::io::Write;
    let mut seed = tempfile::NamedTempFile::new().unwrap();
    writeln!(
        seed,
        "/nix/store/00000000000000000000000000000000-wrix-apple-nix-bridge-1"
    )
    .unwrap();
    let (_, architecture) = system().unwrap();
    let value = serde_json::json!([{
        "id":"a".repeat(64),
        "configuration":{"name":"localhost/worker:latest","descriptor":{"digest":format!("sha256:{}","a".repeat(64))},"creationDate":800_000_001.0},
        "variants":[{"platform":{"os":"linux","architecture":architecture},"digest":format!("sha256:{}","b".repeat(64)),"config":{
            "os":"linux","architecture":architecture,"config":{"Labels":{
                "wrix.nix.shared":"1","wrix.nix.client":"1","wrix.nix.apple.bridge.version":"1",
                "wrix.nix.apple.bridge.executable":"/nix/store/00000000000000000000000000000000-wrix-apple-nix-bridge-1/bin/wrix-apple-nix-bridge"
            }}
        }}]
    }]);
    (seed, value)
}
#[test]
fn apple_image_schema_binds_index_architecture_and_seeded_native_executable() {
    let (seed, value) = fixture();
    let (index, architecture, program) =
        native(&serde_json::to_vec(&value).unwrap(), seed.path()).unwrap();
    assert_eq!(index.as_str(), format!("sha256:{}", "a".repeat(64)));
    assert_eq!(architecture, system().unwrap().1);
    assert!(
        program
            .to_str()
            .unwrap()
            .ends_with("/bin/wrix-apple-nix-bridge")
    );
}
#[test]
fn worker_cannot_select_an_unseeded_or_workspace_bridge_program() {
    let (seed, original) = fixture();
    for program in [
        "/workspace/bridge",
        "/nix/store/11111111111111111111111111111111-peer/bin/wrix-apple-nix-bridge",
        "/nix/store/00000000000000000000000000000000-wrix-apple-nix-bridge-1/../bin/wrix-apple-nix-bridge",
    ] {
        let mut value = original.clone();
        value[0]["variants"][0]["config"]["config"]["Labels"]["wrix.nix.apple.bridge.executable"] =
            serde_json::json!(program);
        assert!(native(&serde_json::to_vec(&value).unwrap(), seed.path()).is_err());
    }
}
#[test]
fn index_alias_and_cross_architecture_variants_do_not_prove_selected_image() {
    let (seed, mut value) = fixture();
    value[0]["id"] = serde_json::json!("latest");
    assert!(native(&serde_json::to_vec(&value).unwrap(), seed.path()).is_err());
    let (seed, mut value) = fixture();
    value[0]["variants"][0]["config"]["architecture"] = serde_json::json!("foreign");
    assert!(native(&serde_json::to_vec(&value).unwrap(), seed.path()).is_err());
}
