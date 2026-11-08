use super::*;

fn candidate() -> Package {
    serde_json::from_value(serde_json::json!({
        "controller": "/nix/store/blue-controller",
        "runtime": "/nix/store/blue-runtime",
        "image": "/nix/store/blue-image.drv",
        "controller_derivation": "/nix/store/blue-controller.drv",
        "runtime_derivation": "/nix/store/blue-runtime.drv",
        "management": "/nix/store/client-management"
    }))
    .unwrap()
}

#[test]
fn preparation_metadata_roundtrips_and_rejects_partial_or_non_derivation_recipes() {
    let package = candidate();
    let value = serde_json::to_value(&package).unwrap();
    assert_eq!(
        serde_json::from_value::<Package>(value.clone()).unwrap(),
        package
    );
    for field in ["controller_derivation", "runtime_derivation", "management"] {
        let mut partial = value.clone();
        partial.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<Package>(partial).is_err());
    }
    let mut invalid = value;
    invalid["runtime_derivation"] = serde_json::json!("/nix/store/not-a-recipe");
    assert!(serde_json::from_value::<Package>(invalid).is_err());
    assert_eq!(
        Native::for_package(Platform::Darwin, 501, &package).nix_store,
        Path::new("/nix/store/client-management/bin/nix-store")
    );
}

#[test]
fn preparation_retains_distinct_consumer_candidates_under_separate_roots() {
    let parent = Path::new("/private/prepared-controller");
    let blue = candidate();
    let mut green = blue.clone();
    green.controller = "/nix/store/green-controller".into();
    assert_ne!(
        blue.preparation_root(parent).unwrap(),
        green.preparation_root(parent).unwrap()
    );
    assert_eq!(
        blue.preparation_root(parent).unwrap(),
        parent.join("blue-controller")
    );
}

#[test]
fn ordinary_start_refuses_unprepared_tooling_without_realization_or_selection() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let log = root.join("nix-log");
    let nix_store = script(
        &root,
        "nix-store",
        &format!(
            "printf '%s\\n' \"$*\" >> '{}'\n[[ \"$1\" == --check-validity ]]\nexit 1",
            log.display()
        ),
    );
    let native = Native::new(
        Platform::Darwin,
        501,
        script(&root, "launchctl", "exit 0"),
        nix_store,
    );
    let lifecycle = Lifecycle::lock(&root.join("controller")).unwrap();
    assert!(matches!(
        lifecycle.start(&native, &candidate()),
        Err(Error::Unprepared)
    ));
    assert!(!lifecycle.root.join("package.json").exists());
    assert!(!lifecycle.root.join("roots").exists());
    assert_eq!(
        fs::read_to_string(log).unwrap(),
        "--check-validity /nix/store/blue-controller /nix/store/blue-runtime\n"
    );
}

#[test]
fn preflight_uses_retained_package_without_preparing_a_new_consumer_candidate() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let lifecycle = Lifecycle::lock(&root.join("controller")).unwrap();
    let retained = package("retained");
    fs::write(
        lifecycle.root.join("package.json"),
        serde_json::to_vec(&retained).unwrap(),
    )
    .unwrap();
    let supervisor = script(
        &root,
        "launchctl",
        &format!(
            "printf '%s\\n' 'path = {}' 'program = {}'",
            lifecycle
                .root
                .join("org.wrix.host-controller.plist")
                .display(),
            retained.executable().display()
        ),
    );
    let native = Native::new(
        Platform::Darwin,
        501,
        supervisor,
        "/must-not-realize-new-candidate".into(),
    );
    assert_eq!(
        lifecycle.preflight(&native, &candidate()).unwrap(),
        Some(retained)
    );
    assert!(!lifecycle.root.join("roots").exists());
}

#[test]
fn explicit_preparation_checks_all_provenance_before_realization() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let log = root.join("nix-log");
    let nix_store = script(
        &root,
        "nix-store",
        &format!(
            r#"
printf '%s\n' "$*" >> '{}'
[[ "$1" == --query && "$2" == --outputs && "$#" == 3 ]]
case "$3" in
/nix/store/blue-controller.drv) printf '%s\n' /nix/store/blue-controller ;;
/nix/store/blue-runtime.drv) printf '%s\n' /nix/store/foreign-runtime ;;
*) exit 91 ;;
esac
"#,
            log.display()
        ),
    );
    let native = Native::new(Platform::Darwin, 501, "/unused".into(), nix_store);
    assert!(matches!(
        native.prepare(&root.join("roots"), &candidate()),
        Err(Error::Provenance)
    ));
    assert!(!root.join("roots").exists());
    assert!(!fs::read_to_string(log).unwrap().contains("--realise"));
}

#[test]
fn explicit_preparation_realizes_pinned_recipes_with_indirect_roots_without_supervision() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let log = root.join("nix-log");
    let nix_store = script(
        &root,
        "nix-store",
        &format!(
            r#"
printf '%s\n' "$*" >> '{}'
case "$1" in
--query)
    [[ "$2" == --outputs && "$#" == 3 ]]
    printf '%s\n' "${{3%.drv}}"
    ;;
--realise)
    [[ "$#" == 5 && "$2" == /nix/store/blue-*.drv && "$3" == --add-root && "$5" == --indirect ]]
    ln -s "${{2%.drv}}" "$4"
    ;;
*) exit 91 ;;
esac
"#,
            log.display()
        ),
    );
    let native = Native::new(
        Platform::Darwin,
        501,
        "/must-not-invoke-supervision".into(),
        nix_store,
    );
    native.prepare(&root.join("roots"), &candidate()).unwrap();
    for name in ["controller", "runtime"] {
        assert_eq!(
            fs::read_link(root.join("roots").join(name)).unwrap(),
            PathBuf::from(format!("/nix/store/blue-{name}"))
        );
    }
    let commands = fs::read_to_string(log).unwrap();
    assert_eq!(
        commands
            .lines()
            .filter(|line| line.starts_with("--realise"))
            .count(),
        2
    );
    assert!(!root.join("package.json").exists());
}
