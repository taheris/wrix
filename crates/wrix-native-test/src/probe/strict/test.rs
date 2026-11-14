use super::*;
use std::os::unix::fs::PermissionsExt;
fn program() -> Program {
    Program::parse(Path::new(
        "/nix/store/00000000000000000000000000000000-wrix-native-test/bin/wrix-native-test",
    ))
    .unwrap()
}
fn fixture(root: &Path, body: &str) -> PathBuf {
    let path = root.join("nix");
    fs::write(
        &path,
        format!(
            "#!/usr/bin/env bash\nset -eu\nprintf '%s\\n' \"$@\" > \"${{0%/*}}/argv\"\n{body}\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}
#[test]
fn command_driver_preserves_strict_flags_and_removes_its_marker() {
    let root = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(root.path()).unwrap();
    let nix = fixture(&root, "printf native-output");
    let marker = root.join("marker");
    let observation = run(&nix, &program(), &marker, System::Arm).unwrap();
    assert!(observation.success);
    assert_eq!(observation.stdout, b"native-output");
    assert!(!marker.exists());
    let args = fs::read_to_string(root.join("argv")).unwrap();
    let args: Vec<_> = args.lines().collect();
    assert_eq!(
        &args[..14],
        &[
            "build",
            "--impure",
            "--no-link",
            "--print-out-paths",
            "--option",
            "sandbox",
            "true",
            "--option",
            "sandbox-fallback",
            "false",
            "--option",
            "max-jobs",
            "1",
            "--expr"
        ]
    );
    assert_eq!(
        args[14],
        expression(&program(), &marker, System::Arm).unwrap()
    );
}
#[test]
fn command_refusal_keeps_real_status_and_non_utf8_diagnostics() {
    let root = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(root.path()).unwrap();
    let nix = fixture(&root, r"printf '\377' >&2; exit 23");
    let result = run(&nix, &program(), &root.join("marker"), System::Intel).unwrap();
    assert!(!result.success);
    assert_eq!(result.exit, Some(23));
    assert_eq!(result.stderr, vec![255]);
    assert!(!root.join("marker").exists());
}
#[test]
fn existing_marker_is_never_overwritten_and_prevents_command_execution() {
    let root = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(root.path()).unwrap();
    let nix = fixture(&root, "exit 0");
    let marker = root.join("marker");
    fs::write(&marker, b"keep").unwrap();
    assert!(run(&nix, &program(), &marker, System::Arm).is_err());
    assert_eq!(fs::read(marker).unwrap(), b"keep");
    assert!(!root.join("argv").exists());
}
#[test]
fn replaced_marker_inode_is_preserved_and_is_not_accepted() {
    let root = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(root.path()).unwrap();
    let nix = fixture(
        &root,
        r#"mv "${0%/*}/marker" "${0%/*}/original"; printf foreign > "${0%/*}/marker""#,
    );
    assert!(matches!(
        run(&nix, &program(), &root.join("marker"), System::Arm),
        Err(Error::Cleanup { .. })
    ));
    assert_eq!(fs::read(root.join("marker")).unwrap(), b"foreign");
    assert_eq!(fs::read(root.join("original")).unwrap(), b"probe");
}
#[test]
fn spawn_failure_still_removes_only_the_owned_marker() {
    let root = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(root.path()).unwrap();
    assert!(
        run(
            &root.join("missing-nix"),
            &program(),
            &root.join("marker"),
            System::Arm
        )
        .is_err()
    );
    assert!(!root.join("marker").exists());
}
#[test]
fn plan_is_inert_retains_store_context_and_quotes_nix_interpolation() {
    let marker = Path::new(r#"/run/${abort "not-evaluated"}"#);
    let expression = expression(&program(), marker, System::Arm).unwrap();
    assert!(expression.contains("builtins.storePath"));
    assert!(expression.contains(r#"\${abort \"not-evaluated\"}"#));
    let plan = plan(&program(), marker, System::Arm).unwrap();
    assert!(!plan.native_execution);
    assert!(plan.sandbox);
    assert!(!plan.sandbox_fallback);
    for path in [
        "/tmp/wrix-native-test",
        "/nix/store/eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-tools/bin/wrix-native-test",
        "/nix/store/00000000000000000000000000000000-tools/bin/other",
    ] {
        assert!(Program::parse(Path::new(path)).is_err());
    }
    assert!(System::parse("aarch64-darwin".as_ref()).is_err());
    assert!(super::expression(&program(), Path::new("relative"), System::Arm).is_err());
}
#[test]
fn host_request_targets_only_the_original_builder_and_explicit_strict_guest() {
    let root = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(root.path()).unwrap();
    let runtime = fixture(&root, "printf guest-output");
    let result = host(
        &runtime,
        Path::new("/nix/store/00000000000000000000000000000000-tools/bin/nix"),
        &program(),
        System::Arm,
    )
    .unwrap();
    assert!(result.success);
    assert_eq!(result.stdout, b"guest-output");
    let args = fs::read_to_string(root.join("argv")).unwrap();
    let args: Vec<_> = args.lines().collect();
    assert_eq!(&args[..3], &["exec", "--interactive", "wrix-builder"]);
    assert_eq!(args[3], program().0.to_str().unwrap());
    assert_eq!(&args[4..7], &["operator-probe", "strict-guest", "--nix"]);
    assert_eq!(
        &args[10..],
        &[
            "--marker",
            "/run/wrix-npfo4-isolation-marker",
            "--system",
            "aarch64-linux",
            "--run"
        ]
    );
}
#[test]
fn host_runtime_failure_is_observed_not_promoted_to_native_nix_success() {
    let root = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(root.path()).unwrap();
    let runtime = fixture(&root, "printf runtime-refusal >&2; exit 23");
    let result = host(
        &runtime,
        Path::new("/nix/store/00000000000000000000000000000000-tools/bin/nix"),
        &program(),
        System::Intel,
    )
    .unwrap();
    assert!(!result.success);
    assert_eq!(result.exit, Some(23));
    assert_eq!(result.stderr, b"runtime-refusal");
    assert!(
        host(
            Path::new("relative"),
            Path::new("/absolute/nix"),
            &program(),
            System::Arm
        )
        .is_err()
    );
}
#[test]
fn marker_is_private_and_writable_shared_parent_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(root.path()).unwrap();
    let marker = Marker::create(&root.join("marker")).unwrap();
    assert_eq!(fs::metadata(&marker.path).unwrap().mode() & 0o777, 0o600);
    marker.remove().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(matches!(
        Marker::create(&root.join("marker")),
        Err(Error::Arguments)
    ));
    fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
}
