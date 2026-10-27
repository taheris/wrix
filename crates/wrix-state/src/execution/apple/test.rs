use super::*;

#[test]
fn native_name_and_birth_are_both_required_to_identify_a_worker() {
    let identity = Identity::parse("wrix-worker-123", 792_000_001.5).unwrap();
    assert_eq!(identity.name(), "wrix-worker-123");
    assert_ne!(
        identity,
        Identity::parse("wrix-worker-123", 792_000_001.75).unwrap()
    );
    assert_ne!(
        identity,
        Identity::parse("wrix-worker-124", 792_000_001.5).unwrap()
    );
    let bytes = serde_json::to_vec(&identity).unwrap();
    assert_eq!(
        serde_json::from_slice::<Identity>(&bytes).unwrap(),
        identity
    );
}

#[test]
fn creation_dates_must_be_positive_finite_native_values() {
    for birth in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, -0.0, 0.0] {
        assert!(Identity::parse("wrix-worker", birth).is_err());
    }
}

#[test]
fn native_names_cannot_select_arguments_paths_or_unbounded_objects() {
    for name in [
        "",
        "--all",
        "../worker",
        "/worker",
        "a b",
        "a\nb",
        &"x".repeat(64),
    ] {
        assert!(Identity::parse(name, 1.0).is_err());
    }
}

#[test]
fn apple_phases_never_deserialize_as_podman_binding_or_elapsed_exit() {
    let identity = Identity::parse("wrix-worker-123", 792_000_001.5).unwrap();
    for phase in [
        Phase::AppleBound(identity.clone()),
        Phase::AppleRetired(identity),
    ] {
        let bytes = serde_json::to_vec(&phase).unwrap();
        assert_eq!(serde_json::from_slice::<Phase>(&bytes).unwrap(), phase);
    }
    for value in [
        r#"{"apple-bound":{"name":"worker","born":0}}"#,
        r#"{"apple-retired":"worker"}"#,
        r#"{"bound":{"name":"worker","born":1}}"#,
    ] {
        assert!(serde_json::from_str::<Phase>(value).is_err());
    }
}
