use super::*;
fn fixture() -> (Digest, Compatibility, Observation) {
    let digest = Digest::parse(&format!("sha256:{}", "a".repeat(64))).unwrap();
    let compatibility = Compatibility {
        version: wrix_core::nix_store::Version::CURRENT,
        seed_closure: wrix_core::nix_store::SeedClosure::parse(
            "/nix/store/00000000000000000000000000000000-closure-info/store-paths",
        )
        .unwrap(),
    };
    let hash = format!(
        "{:x}",
        Sha256::digest(
            compatibility
                .seed_closure
                .as_path()
                .as_os_str()
                .as_encoded_bytes()
        )
    );
    let observation = serde_json::from_value(serde_json::json!({ "Id": digest.as_str(), "Architecture":"amd64", "Os":"linux", "Config":{"Labels":{"wrix.managed":"true", "wrix.image.kind":"profile", "wrix.nix.private":"1", "wrix.nix.shared":"1", "wrix.nix.seed":hash}}})).unwrap();
    (digest, compatibility, observation)
}
#[test]
fn seed_binding_requires_exact_native_image_architecture_and_factory_closure_label() {
    let (digest, compatibility, mut observation) = fixture();
    assert!(Image::binding(System::X86_64Linux, &digest, &compatibility, &observation).is_ok());
    assert!(Image::binding(System::Aarch64Linux, &digest, &compatibility, &observation).is_err());
    let different = Digest::parse(&format!("sha256:{}", "b".repeat(64))).unwrap();
    assert!(
        Image::binding(
            System::X86_64Linux,
            &different,
            &compatibility,
            &observation
        )
        .is_err()
    );
    observation
        .configuration
        .labels
        .insert("wrix.nix.seed".into(), "b".repeat(64));
    assert!(Image::binding(System::X86_64Linux, &digest, &compatibility, &observation).is_err());
}
#[test]
fn seed_paths_are_bounded_exact_and_cannot_select_host_files_or_commands() {
    let input = b"/nix/store/00000000000000000000000000000000-software\n";
    assert_eq!(Image::paths(input).unwrap().len(), 1);
    for value in [
        b"/etc/shadow\n".to_vec(),
        b"../store-paths\n".to_vec(),
        [input.as_slice(), input.as_slice()].concat(),
        b"/nix/store/00000000000000000000000000000000-software".to_vec(),
    ] {
        assert!(Image::paths(&value).is_err());
    }
}
