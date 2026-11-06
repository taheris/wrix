use super::*;
use std::os::unix::fs::PermissionsExt;

const RUNNER: &str = r"
match = '^verify:(.+)$'
command = 'nix run .#verify -- {targets}'
target = '{capture_1}'
join = ' '
parse = 'json-lines'
cwd = '.'
skip_policy = 'sandbox-capability'
skip_capabilities = ['container-runtime', 'kvm', 'user-network-namespace']
inputs = 'nix run .#verify -- {print_inputs} {targets}'
";

#[test]
fn runner_policy_audit_accepts_valid_policy_and_rejects_every_missing_field() {
    let valid: toml::Value = toml::from_str(RUNNER).unwrap();
    assert_eq!(runner_errors(&valid, false), Vec::<String>::new());
    for field in [
        "skip_policy",
        "skip_capabilities",
        "parse",
        "command",
        "inputs",
        "match",
        "target",
        "join",
        "cwd",
    ] {
        let mut broken = valid.clone();
        broken.as_table_mut().unwrap().remove(field);
        assert_ne!(
            runner_errors(&broken, false),
            Vec::<String>::new(),
            "missing {field}"
        );
    }
}

#[test]
fn test_ci_policy_requires_its_own_commands_and_extra_capability() {
    let verify: toml::Value = toml::from_str(RUNNER).unwrap();
    assert_ne!(runner_errors(&verify, true), Vec::<String>::new());
    let raw = RUNNER
        .replace("^verify:", "^test-ci:")
        .replace("nix run .#verify --", "bin/test-ci-verifiers")
        .replace(
            "'user-network-namespace']",
            "'user-network-namespace', 'virtiofsd-capabilities']",
        );
    let mut test_ci: toml::Value = toml::from_str(&raw).unwrap();
    assert_eq!(runner_errors(&test_ci, true), Vec::<String>::new());
    test_ci["skip_capabilities"]
        .as_array_mut()
        .unwrap()
        .reverse();
    assert_ne!(runner_errors(&test_ci, true), Vec::<String>::new());
}

#[test]
fn wrong_policy_field_types_are_rejected() {
    let mut value: toml::Value = toml::from_str(RUNNER).unwrap();
    value["skip_policy"] = toml::Value::Boolean(true);
    value["skip_capabilities"] = toml::Value::String("container-runtime".to_owned());
    assert_eq!(runner_errors(&value, false).len(), 2);
}

#[test]
fn source_annotation_audit_preserves_all_original_selector_cases() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let spec = root.join("specs/synthetic.md");
    fs::create_dir_all(spec.parent().unwrap()).unwrap();
    for name in ["crates/example/tests/present.rs", "tests/judges/present.sh"] {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "fixture source\n").unwrap();
    }
    for (tier, directory, suffix) in [
        ("test", "crates/example/tests", ".rs::test_name"),
        ("judge", "tests/judges", ".sh#rubric"),
        ("judge", "tests/judges", ".sh::rubric"),
    ] {
        for pending in [false, true] {
            for name in ["present", "missing"] {
                let target = format!("../{directory}/{name}{suffix}");
                let marker = if pending { "?" } else { "" };
                let text = format!("- Criterion [{tier}{marker}]({target})\n");
                let found = annotations(&text, true);
                assert_eq!(
                    found,
                    [Annotation {
                        target: &target,
                        pending
                    }]
                );
                let error = file_selector_error(root, &spec, found[0].target, found[0].pending);
                if name == "missing" && !pending {
                    assert!(error.unwrap().contains("resolves to missing file"));
                } else {
                    assert!(error.is_none(), "{text}: {error:?}");
                }
            }
            let target = "crates/example/tests/present.rs::test_name";
            let error = file_selector_error(root, &spec, target, pending).unwrap();
            assert!(error.contains("workspace-relative"));
        }
    }
    let found = annotations("- Criterion [test?](module::planned_test)\n", true);
    assert_eq!(
        found,
        [Annotation {
            target: "module::planned_test",
            pending: true
        }]
    );
    assert!(file_selector_error(root, &spec, found[0].target, true).is_none());
}

#[test]
fn pending_logical_annotation_targets_are_ignored() {
    assert_eq!(
        annotations(
            "[check?](verify:missing.target) [system?](verify:missing.other)",
            false
        ),
        Vec::<Annotation<'_>>::new()
    );
}

#[test]
fn logical_annotation_targets_require_the_real_registry() {
    let spec = Path::new("specs/synthetic.md");
    let inventory = BTreeSet::from(["verify:present.target".to_owned()]);
    assert!(
        logical_annotation_error(spec, "verify:missing.target", &inventory)
            .unwrap()
            .contains("absent from .#verify --list")
    );
    assert!(logical_annotation_error(spec, "verify:present.target", &inventory).is_none());
    let found = annotations(
        "[check](verify:present.target) [system](verify:missing.target) [check](not-a-verifier)",
        false,
    );
    assert_eq!(found.len(), 2);
    assert_eq!(found[1].target, "verify:missing.target");
}

#[test]
fn file_selector_adapter_executes_the_real_source_adapter() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    check_adapter(&root).unwrap();
}

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join("specs")).unwrap();
    fs::create_dir_all(temp.path().join("tests")).unwrap();
    fs::write(
        temp.path().join("loom.toml"),
        include_str!("../../../../loom.toml"),
    )
    .unwrap();
    let adapter = temp.path().join("tests/loom-nextest-file-targets.sh");
    fs::write(
        &adapter,
        include_str!("../../../../tests/loom-nextest-file-targets.sh"),
    )
    .unwrap();
    fs::set_permissions(adapter, fs::Permissions::from_mode(0o755)).unwrap();
    temp
}

#[test]
fn packaged_policy_and_actual_adapter_pass_the_end_to_end_audit() {
    let temp = fixture();
    fs::write(
        temp.path().join("specs/synthetic.md"),
        "[check](verify:present.target) [test?](module::future)\n",
    )
    .unwrap();
    audit(&temp.path().join("loom.toml"), "\nverify:present.target\n").unwrap();
}

#[test]
fn empty_inventory_is_not_accepted_as_success() {
    let temp = fixture();
    let error = audit(&temp.path().join("loom.toml"), " \n\t\n")
        .unwrap_err()
        .to_string();
    assert!(error.contains("inventory is empty"));
}

#[test]
fn missing_spec_target_is_rejected_by_the_end_to_end_audit() {
    let temp = fixture();
    fs::write(
        temp.path().join("specs/synthetic.md"),
        "[check](verify:missing.target)\n",
    )
    .unwrap();
    let error = audit(&temp.path().join("loom.toml"), "verify:present.target\n")
        .unwrap_err()
        .to_string();
    assert!(error.contains("verify:missing.target"));
}

#[test]
fn duplicated_registry_targets_are_rejected_even_in_comments() {
    for duplicate in DUPLICATED {
        let temp = fixture();
        let config = temp.path().join("loom.toml");
        let raw = format!("{}\n# {duplicate}\n", fs::read_to_string(&config).unwrap());
        fs::write(&config, raw).unwrap();
        let error = audit(&config, "verify:present.target\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("duplicates verifier target"));
    }
}

#[test]
fn missing_runner_sections_and_test_command_fail_closed() {
    for (raw, expected) in [
        ("", "missing [runner]"),
        ("[runner]", "missing [runner.check.verify]"),
    ] {
        let temp = fixture();
        let config = temp.path().join("loom.toml");
        fs::write(&config, raw).unwrap();
        assert!(
            audit(&config, "verify:present.target\n")
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
    }
    let temp = fixture();
    let config = temp.path().join("loom.toml");
    let raw = fs::read_to_string(&config)
        .unwrap()
        .replace(TEST_RUNNER, "unapproved-test-runner");
    fs::write(&config, raw).unwrap();
    assert!(
        audit(&config, "verify:present.target\n")
            .unwrap_err()
            .to_string()
            .contains("[runner.test] command must be")
    );
}

#[test]
fn adapter_failure_is_not_ignored() {
    let temp = fixture();
    fs::write(
        temp.path().join("tests/loom-nextest-file-targets.sh"),
        "#!/usr/bin/env bash\necho adapter-failed >&2\nexit 23\n",
    )
    .unwrap();
    let error = audit(&temp.path().join("loom.toml"), "verify:present.target\n")
        .unwrap_err()
        .to_string();
    assert!(error.contains("adapter-failed"));
}
