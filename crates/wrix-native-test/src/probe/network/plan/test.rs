use super::*;
fn inputs(program: &Program) -> Inputs<'_> {
    Inputs { program, certificate: Certificate::parse(Path::new("/nix/store/00000000000000000000000000000000-nss-cacert-3.126/etc/ssl/certs/ca-bundle.crt")).unwrap(), public: Public::parse("https://cache.nixos.org/nix-cache-info").unwrap(), private: Private::parse("http://10.231.47.1:24823").unwrap(), expected: Hash::parse(&"a".repeat(64)).unwrap(), system: System::Arm }
}
#[test]
fn inert_plan_keeps_actual_rust_builders_certificate_inputs_and_fixed_hash_policy() {
    let program = Program::parse(Path::new(
        "/nix/store/00000000000000000000000000000000-native/bin/wrix-native-test",
    ))
    .unwrap();
    let inputs = inputs(&program);
    let plan = inputs.plan().unwrap();
    assert!(!plan.native_execution);
    assert!(!plan.ordinary.contains("outputHash"));
    assert!(
        plan.fixed_output
            .contains("outputHashAlgo=\"sha256\"; outputHashMode=\"flat\";")
    );
    assert!(
        plan.fixed_output
            .contains(&format!("outputHash=\"{}\"", "a".repeat(64)))
    );
    for expression in [&plan.ordinary, &plan.fixed_output] {
        assert_eq!(expression.matches("builtins.storePath").count(), 2);
        assert!(expression.contains("certificateRoot=ca"));
        assert!(expression.contains("builder=probe + \"/bin/wrix-native-test\""));
        assert!(expression.contains("\"operator-probe\" \"network-worker\""));
        assert!(!expression.contains("python"));
    }
    assert!(plan.ordinary.contains("\"pure\""));
    assert!(plan.fixed_output.contains("\"fixed\""));
}
#[test]
fn certificate_must_be_the_exact_declared_bundle_not_an_ambient_file() {
    for path in [
        "/etc/ssl/certs/ca-bundle.crt",
        "/nix/store/00000000000000000000000000000000-nss-cacert/etc/ssl/certs/other",
        "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-nss-cacert/etc/ssl/certs/ca-bundle.crt",
        "/nix/store/00000000000000000000000000000000-other/etc/ssl/certs/ca-bundle.crt",
    ] {
        assert!(Certificate::parse(Path::new(path)).is_err());
    }
}
#[test]
fn incomplete_plan_arguments_cannot_execute_any_network_or_namespace_operation() {
    assert!(matches!(entry(&[]), Err(Error::Usage)));
}
