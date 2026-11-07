//! Real dispatcher and consumed Loom result/skip protocol conformance.
#![cfg(test)]
#[path = "common/verifier/mod.rs"]
mod fixture;

use fixture::{Sandbox, capture, combined, executable, git_locals, quote, records, text};
use serde_json::{Value, json};
use std::{
    env,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::Output,
};

const PASS: &str = "notifications.focus-target-envelope";
const FAIL: &str = "notifications.client-non-blocking";
const UNEXPECTED: &str = "notifications.client-tcp-endpoint-override";
const BUILDER: &str = "linux-builder.integration";
fn empty() -> Value {
    json!({"platforms":[],"capabilities":[]})
}
fn binary(name: &str) -> PathBuf {
    env::var_os(name).unwrap().into()
}

struct Fixture {
    sandbox: Sandbox,
    platform: String,
}
impl Fixture {
    fn new() -> Self {
        let sandbox = Sandbox::new("wrix-verifier-results-");
        sandbox.write("tests/standalone/notify-test.sh", r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$1" >>"$REPO_ROOT/calls"
case "$1" in
  test_focus_target_envelope) printf 'fixture passed\n' ;;
  test_client_non_blocking) printf 'fixture assertion failed\n' >&2; false; printf 'concealed failure\n' ;;
  test_client_tcp_endpoint_override) printf 'unreported fixture prerequisite\n' >&2; exit 77 ;;
  *) printf 'preflight should not execute this fixture\n' >&2; exit 24 ;;
esac
"#);
        let result = capture(
            sandbox
                .command("bash")
                .args(["-c", "source \"$1\"; verifier_platform", "fixture"])
                .arg(binary("WRIX_TEST_VERIFIER_LIBRARY")),
        );
        assert!(result.status.success(), "{result:?}");
        Self {
            sandbox,
            platform: text(&result.stdout).trim().into(),
        }
    }
    fn foreign(&self) -> &'static str {
        if self.platform.ends_with("-linux") {
            "notifications.container-transport-darwin"
        } else {
            "notifications.container-transport-linux"
        }
    }
    fn execution(&self) -> Value {
        let mut value = empty();
        value["platform"] = self.platform.clone().into();
        value
    }
    fn run(&self, targets: &[&str]) -> (Output, Vec<Value>) {
        let result = capture(
            self.sandbox
                .command(binary("WRIX_TEST_VERIFY"))
                .args(targets),
        );
        let records = records(&result.stdout);
        (result, records)
    }
    fn builder(&mut self, platform: &str, version: &str, status: i32) {
        self.sandbox.write("bin/uname", "");
        executable(
            &self.sandbox.root.join("bin/uname"),
            r#"#!/usr/bin/env bash
set -euo pipefail
case "${1-}" in
  ""|-s) printf '%s\n' "$BUILDER_PLATFORM" ;;
  -m) printf 'aarch64\n' ;;
  *) exit 64 ;;
esac
"#,
        );
        executable(
            &self.sandbox.root.join("bin/sw_vers"),
            "#!/usr/bin/env bash\nset -euo pipefail\n[[ \"$*\" == -productVersion ]] || exit 64\nprintf '%s\\n' \"$BUILDER_VERSION\"\n",
        );
        self.sandbox.write("platform.sh", "set -euo pipefail\nuname() { \"$REPO_ROOT/bin/uname\" \"$@\"; }\nsw_vers() { \"$REPO_ROOT/bin/sw_vers\" \"$@\"; }\n");
        self.sandbox.write("tests/standalone/builder-test.sh", "#!/usr/bin/env bash\nset -euo pipefail\nprintf 'builder integration invoked\\n' >>\"$REPO_ROOT/calls\"\nprintf '%s\\n' \"$BUILDER_EVIDENCE\" >&2\nexit \"$BUILDER_STATUS\"\n");
        for (key, value) in [
            (
                "BASH_ENV",
                self.sandbox.root.join("platform.sh").into_os_string(),
            ),
            ("BUILDER_PLATFORM", platform.into()),
            ("BUILDER_VERSION", version.into()),
            ("BUILDER_STATUS", status.to_string().into()),
            ("BUILDER_EVIDENCE", "fixture builder prerequisite".into()),
            ("LOOM_INSIDE", "1".into()),
        ] {
            self.sandbox.environment.insert(key.into(), value);
        }
    }
    fn real_builder(&self) {
        self.sandbox.write(
            "tests/standalone/builder-test.sh",
            &fs::read_to_string(binary("WRIX_TEST_BUILDER_SUITE")).unwrap(),
        );
    }
    fn library(&self, requirements: &Value, prefix: Option<&Path>) -> (Output, Value) {
        let mut command = self.sandbox.command("bash");
        command.args(["-c", "source \"$1\"; verifier_run fixture \"$2\" bash -c 'echo ran >\"$REPO_ROOT/executed\"'", "fixture"])
            .arg(binary("WRIX_TEST_VERIFIER_LIBRARY")).arg(requirements.to_string());
        if let Some(prefix) = prefix {
            command.env(
                "PATH",
                env::join_paths(
                    std::iter::once(prefix.to_owned()).chain(env::split_paths(
                        self.sandbox
                            .environment
                            .get(std::ffi::OsStr::new("PATH"))
                            .unwrap(),
                    )),
                )
                .unwrap(),
            );
        }
        let result = capture(&mut command);
        let record = serde_json::from_slice(&result.stdout).unwrap();
        (result, record)
    }
    fn gate(&self, targets: &[&str], worker: bool, tier: &str) -> Output {
        let mut annotations = String::new();
        for (index, target) in targets.iter().enumerate() {
            writeln!(annotations, "- Target {index} [{tier}](verify:{target})").unwrap();
        }
        self.sandbox.write(
            "specs/fixture.md",
            &format!("# Fixture\n\n## Success Criteria\n\n{annotations}"),
        );
        let command = serde_json::to_string(&format!(
            "{} {{targets}}",
            quote(binary("WRIX_TEST_VERIFY"))
        ))
        .unwrap();
        self.sandbox.write("loom.toml", &format!("[runner.{tier}.verify]\nmatch = '^verify:(.+)$'\ncommand = {command}\ntarget = '{{capture_1}}'\njoin = ' '\nparse = 'json-lines'\ncwd = '.'\nskip_policy = 'sandbox-capability'\nskip_capabilities = ['container-runtime', 'kvm', 'user-network-namespace']\n"));
        let mut environment = self.sandbox.environment.clone();
        environment.remove(std::ffi::OsStr::new("LOOM_INSIDE"));
        if worker {
            environment.insert("LOOM_INSIDE".into(), "1".into());
        }
        for name in git_locals() {
            environment.remove(std::ffi::OsStr::new(&name));
        }
        for args in [
            vec!["init", "-q"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "Create fixture",
            ],
        ] {
            let result = capture(
                self.sandbox
                    .command("git")
                    .current_dir(&self.sandbox.root)
                    .env_clear()
                    .envs(&environment)
                    .args(args),
            );
            assert!(result.status.success(), "{result:?}");
        }
        capture(
            self.sandbox
                .command(binary("WRIX_TEST_LOOM"))
                .current_dir(&self.sandbox.root)
                .env_clear()
                .envs(environment)
                .args(["gate", "verify", "--workspace"])
                .arg(&self.sandbox.root)
                .arg("--tree"),
        )
    }
    fn assert_worker_builder_skip(&self, result: &Output) {
        assert_eq!(result.status.code(), Some(1), "{result:?}");
        assert!(
            combined(result).contains("coverage unverified, permitted=false"),
            "{result:?}"
        );
        let records = records(
            self.sandbox
                .read(".loom/logs/gate/verifier-results.jsonl")
                .as_bytes(),
        );
        let builders: Vec<_> = records
            .iter()
            .filter(|record| record["target"] == format!("verify:{BUILDER}"))
            .collect();
        assert_eq!(builders.len(), 1);
        let record = builders[0];
        assert_eq!(record["outcome"], "skipped");
        assert_eq!(record["coverage_verified"], false);
        assert_eq!(record["result"]["pass"], false);
        assert!(record["result"].get("skip_reason").is_none());
        assert!(
            self.sandbox
                .read("calls")
                .contains("builder integration invoked")
        );
        assert!(!self.sandbox.root.join(".loom/marker.json").exists());
    }
}
fn assert_builder_skip(record: &Value, platform: &str) {
    assert_eq!(record["target"], BUILDER);
    assert_eq!(record["pass"], false);
    assert_eq!(record["skipped"], true);
    assert!(record.get("outcome").is_none());
    assert!(record.get("skip_reason").is_none());
    let mut execution = empty();
    execution["platform"] = platform.into();
    assert_eq!(record["execution"], execution);
    assert!(
        record["evidence"]
            .as_str()
            .unwrap()
            .contains("unexpected skip:")
    );
}

#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn pass_reports_execution_and_omits_skip_reason() {
    let fixture = Fixture::new();
    let (result, records) = fixture.run(&[PASS]);
    assert!(result.status.success(), "{result:?}");
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0],
        json!({"target":PASS,"outcome":"passed","evidence":"passed","execution":fixture.execution()})
    );
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn target_prefixes_and_duplicate_invocations_are_preserved() {
    let fixture = Fixture::new();
    let (result, records) = fixture.run(&[PASS, &format!("verify:{PASS}")]);
    assert!(result.status.success(), "{result:?}");
    assert_eq!(
        records
            .iter()
            .map(|record| record["target"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [PASS, PASS]
    );
    assert_eq!(
        fixture.sandbox.read("calls").lines().collect::<Vec<_>>(),
        ["test_focus_target_envelope"; 2]
    );
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn intermediate_assertion_failure_cannot_be_hidden_by_later_success() {
    let fixture = Fixture::new();
    let (result, records) = fixture.run(&[FAIL, PASS]);
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(
        records
            .iter()
            .map(|record| record["outcome"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["failed", "passed"]
    );
    assert!(
        records[0]["evidence"]
            .as_str()
            .unwrap()
            .contains("fixture assertion failed"),
        "{records:#?}"
    );
    assert!(
        !records[0]["evidence"]
            .as_str()
            .unwrap()
            .contains("concealed failure")
    );
    assert!(records[0].get("skip_reason").is_none());
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn foreign_platform_skips_before_invocation() {
    let fixture = Fixture::new();
    let (result, records) = fixture.run(&[fixture.foreign()]);
    assert_eq!(result.status.code(), Some(77), "{result:?}");
    let record = &records[0];
    assert_eq!(record["outcome"], "skipped");
    assert_eq!(record["execution"]["platform"], fixture.platform);
    assert!(
        !record["execution"]["platforms"]
            .as_array()
            .unwrap()
            .contains(&json!(fixture.platform))
    );
    assert_eq!(record["skip_reason"]["kind"], "foreign-platform");
    assert_ne!(record["skip_reason"]["reason"].as_str().unwrap(), "");
    assert_eq!(fixture.sandbox.read("calls"), "");
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn pass_does_not_conceal_skip() {
    let fixture = Fixture::new();
    let (result, records) = fixture.run(&[PASS, fixture.foreign()]);
    assert_eq!(result.status.code(), Some(77), "{result:?}");
    assert_eq!(
        records
            .iter()
            .map(|record| record["outcome"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["passed", "skipped"]
    );
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn skip_does_not_conceal_failure() {
    let fixture = Fixture::new();
    let (result, records) = fixture.run(&[fixture.foreign(), FAIL]);
    assert_eq!(result.status.code(), Some(1), "{result:?}");
    assert_eq!(
        records
            .iter()
            .map(|record| record["outcome"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["skipped", "failed"]
    );
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn unreported_exit_77_stays_unauthorized_legacy_skip() {
    let fixture = Fixture::new();
    let (result, records) = fixture.run(&[PASS, UNEXPECTED]);
    assert_eq!(result.status.code(), Some(77));
    let record = &records[1];
    assert_eq!(record["pass"], false);
    assert_eq!(record["skipped"], true);
    assert!(record.get("skip_reason").is_none());
    assert_eq!(record["execution"]["capabilities"], json!([]));
    assert!(
        record["evidence"]
            .as_str()
            .unwrap()
            .contains("unreported fixture prerequisite")
    );
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn unknown_target_fails_before_any_invocation() {
    let fixture = Fixture::new();
    let (result, records) = fixture.run(&[PASS, "verify:cli.not-registered"]);
    assert_eq!(result.status.code(), Some(64));
    assert_eq!(records, Vec::<Value>::new());
    assert_eq!(fixture.sandbox.read("calls"), "");
    for expected in [
        "Unknown verify target: verify:cli.not-registered",
        "nix run .#verify -- --list",
        "verify:cli.package-surface",
    ] {
        assert!(text(&result.stderr).contains(expected), "{result:?}");
    }
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn builder_non_darwin_preserves_exit_77() {
    let mut fixture = Fixture::new();
    fixture.builder("Linux", "26.0", 77);
    let (result, records) = fixture.run(&[BUILDER]);
    assert_eq!(result.status.code(), Some(77), "{result:?}");
    assert_eq!(records.len(), 1);
    assert_builder_skip(&records[0], "aarch64-linux");
    assert!(
        records[0]["evidence"]
            .as_str()
            .unwrap()
            .contains("fixture builder prerequisite")
    );
    assert_eq!(
        fixture.sandbox.read("calls").lines().collect::<Vec<_>>(),
        ["builder integration invoked"]
    );
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn builder_older_darwin_preserves_exit_77() {
    let mut fixture = Fixture::new();
    fixture.builder("Darwin", "25.6.1", 77);
    let (result, records) = fixture.run(&[BUILDER]);
    assert_eq!(result.status.code(), Some(77), "{result:?}");
    assert_builder_skip(&records[0], "aarch64-darwin");
    assert_eq!(
        fixture.sandbox.read("calls").lines().collect::<Vec<_>>(),
        ["builder integration invoked"]
    );
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn builder_supported_darwin_executes_suite_successfully() {
    let mut fixture = Fixture::new();
    for version in ["26.0", "27.1"] {
        fixture.builder("Darwin", version, 0);
        fixture.sandbox.write("calls", "");
        let (result, records) = fixture.run(&[BUILDER]);
        assert!(result.status.success(), "{result:?}");
        assert_eq!(records[0]["outcome"], "passed");
        assert_eq!(records[0]["execution"]["platform"], "aarch64-darwin");
        assert_eq!(
            fixture.sandbox.read("calls").lines().collect::<Vec<_>>(),
            ["builder integration invoked"]
        );
    }
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn builder_supported_darwin_propagates_actionable_suite_failure() {
    let mut fixture = Fixture::new();
    fixture.builder("Darwin", "26.0", 23);
    let evidence = "FAIL: builder SSH unavailable; run wrix-builder setup";
    fixture
        .sandbox
        .environment
        .insert("BUILDER_EVIDENCE".into(), evidence.into());
    let direct = capture(
        fixture
            .sandbox
            .command(binary("WRIX_TEST_VERIFY"))
            .args(["--execute-target", BUILDER]),
    );
    assert_eq!(direct.status.code(), Some(23), "{direct:?}");
    let (result, records) = fixture.run(&[BUILDER]);
    assert_eq!(result.status.code(), Some(1), "{result:?}");
    assert_eq!(records[0]["outcome"], "failed");
    assert!(records[0]["evidence"].as_str().unwrap().contains(evidence));
    assert!(records[0].get("skip_reason").is_none());
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn builder_unsupported_platform_rejects_non_skip_suite_results() {
    let mut fixture = Fixture::new();
    for (platform, version) in [("Linux", "26.0"), ("Darwin", "25.6")] {
        for status in [0, 23] {
            fixture.builder(platform, version, status);
            let (result, records) = fixture.run(&[BUILDER]);
            assert_eq!(result.status.code(), Some(1), "{result:?}");
            assert_eq!(records[0]["outcome"], "failed");
            let evidence = records[0]["evidence"].as_str().unwrap();
            assert!(evidence.contains(&format!("builder integration exited {status}")));
            assert!(evidence.contains("expected unsupported-platform skip"));
        }
    }
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn builder_skip_fixture_conforms_to_real_suite() {
    let mut fixture = Fixture::new();
    for (platform, version, reason) in [
        ("Linux", "26.0", "This test only runs on Darwin"),
        ("Darwin", "25.6.1", "Requires macOS 26+ (current: 25.6.1)"),
    ] {
        fixture.builder(platform, version, 77);
        let (original, original_records) = fixture.run(&[BUILDER]);
        assert_eq!(original.status.code(), Some(77), "{original:?}");
        fixture.real_builder();
        let direct = capture(
            fixture.sandbox.command("bash").arg(
                fixture
                    .sandbox
                    .root
                    .join("tests/standalone/builder-test.sh"),
            ),
        );
        assert_eq!(direct.status.code(), original.status.code(), "{direct:?}");
        assert!(text(&direct.stdout).contains(&format!("SKIP: {reason}")));
        let (result, records) = fixture.run(&[BUILDER]);
        assert_eq!(result.status.code(), original.status.code(), "{result:?}");
        assert_builder_skip(&records[0], &format!("aarch64-{}", platform.to_lowercase()));
        assert_eq!(records[0]["execution"], original_records[0]["execution"]);
        assert!(
            records[0]["evidence"]
                .as_str()
                .unwrap()
                .contains(&format!("SKIP: {reason}"))
        );
    }
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn builder_real_suite_invalid_version_remains_failure() {
    let mut fixture = Fixture::new();
    fixture.builder("Darwin", "invalid", 77);
    fixture.real_builder();
    let (result, records) = fixture.run(&[BUILDER]);
    assert_eq!(result.status.code(), Some(1), "{result:?}");
    assert_eq!(records[0]["outcome"], "failed");
    assert!(
        records[0]["evidence"]
            .as_str()
            .unwrap()
            .contains("Could not parse macOS version: invalid")
    );
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn builder_pass_cannot_conceal_skip_in_either_batch_order() {
    let mut fixture = Fixture::new();
    fixture.builder("Linux", "26.0", 77);
    for targets in [[PASS, BUILDER], [BUILDER, PASS]] {
        let (result, records) = fixture.run(&targets);
        assert_eq!(result.status.code(), Some(77), "{result:?}");
        assert_eq!(
            records
                .iter()
                .map(|record| record["target"].as_str().unwrap())
                .collect::<Vec<_>>(),
            targets
        );
        assert_eq!(
            records.iter().find(|r| r["target"] == PASS).unwrap()["outcome"],
            "passed"
        );
        assert_builder_skip(
            records.iter().find(|r| r["target"] == BUILDER).unwrap(),
            "aarch64-linux",
        );
    }
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn builder_skip_cannot_conceal_failure_in_either_batch_order() {
    let mut fixture = Fixture::new();
    fixture.builder("Darwin", "25.6", 77);
    for targets in [[FAIL, BUILDER], [BUILDER, FAIL]] {
        let (result, records) = fixture.run(&targets);
        assert_eq!(result.status.code(), Some(1), "{result:?}");
        assert_eq!(
            records
                .iter()
                .map(|record| record["target"].as_str().unwrap())
                .collect::<Vec<_>>(),
            targets
        );
        let failed = records.iter().find(|r| r["target"] == FAIL).unwrap();
        assert_eq!(failed["outcome"], "failed");
        assert!(
            failed["evidence"]
                .as_str()
                .unwrap()
                .contains("fixture assertion failed"),
            "{records:#?}"
        );
        assert_builder_skip(
            records.iter().find(|r| r["target"] == BUILDER).unwrap(),
            "aarch64-darwin",
        );
    }
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn missing_runtime_skips_before_execution_on_applicable_platform() {
    let fixture = Fixture::new();
    let bin = fixture.sandbox.root.join("bin");
    fs::create_dir(&bin).unwrap();
    for tool in ["bash", "uname", "jq", "mktemp", "head", "cat", "rm"] {
        let result = capture(fixture.sandbox.command("bash").args([
            "-c",
            "command -v \"$1\"",
            "fixture",
            tool,
        ]));
        assert!(result.status.success());
        std::os::unix::fs::symlink(text(&result.stdout).trim(), bin.join(tool)).unwrap();
    }
    let requirement = json!({"platforms":[fixture.platform],"capabilities":["container-runtime"]});
    let result = capture(
        fixture
            .sandbox
            .command(bin.join("bash"))
            .args([
                "-c",
                "source \"$1\"; verifier_run fixture \"$2\" bash -c 'exit 24'",
                "fixture",
            ])
            .arg(binary("WRIX_TEST_VERIFIER_LIBRARY"))
            .arg(requirement.to_string())
            .env("PATH", &bin),
    );
    assert_eq!(result.status.code(), Some(77), "{result:?}");
    let record: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(record["outcome"], "skipped");
    assert_eq!(record["execution"]["platforms"], json!([fixture.platform]));
    assert_eq!(record["skip_reason"]["kind"], "missing-capability");
    assert_eq!(record["skip_reason"]["capability"], "container-runtime");
    assert!(
        record["skip_reason"]["reason"]
            .as_str()
            .unwrap()
            .contains("not on PATH")
    );
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn kvm_preflight_retains_declared_requirement_and_device_reason() {
    let fixture = Fixture::new();
    let (result, record) = fixture.library(&json!({"platforms":[],"capabilities":["kvm"]}), None);
    let available = capture(
        fixture
            .sandbox
            .command("bash")
            .args(["-c", "[[ -r /dev/kvm && -w /dev/kvm && -c /dev/kvm ]]"]),
    )
    .status
    .success();
    assert_eq!(
        result.status.code(),
        Some(if available { 0 } else { 77 }),
        "{result:?}"
    );
    assert_eq!(record["execution"]["capabilities"], json!(["kvm"]));
    assert_eq!(fixture.sandbox.root.join("executed").exists(), available);
    if !available {
        assert_eq!(record["skip_reason"]["capability"], "kvm");
        assert!(
            record["skip_reason"]["reason"]
                .as_str()
                .unwrap()
                .contains("/dev/kvm")
        );
    }
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn namespace_preflight_checks_external_capability_before_execution() {
    let fixture = Fixture::new();
    let bin = fixture.sandbox.root.join("bin");
    fs::create_dir(&bin).unwrap();
    executable(
        &bin.join("unshare"),
        "#!/usr/bin/env bash\nset -euo pipefail\necho fixture-namespace-unavailable >&2\nexit 1\n",
    );
    let (result, record) = fixture.library(
        &json!({"platforms":[],"capabilities":["user-network-namespace"]}),
        Some(&bin),
    );
    assert_eq!(result.status.code(), Some(77));
    assert_eq!(
        record["skip_reason"]["capability"],
        "user-network-namespace"
    );
    assert!(text(&result.stderr).contains("fixture-namespace-unavailable"));
    assert!(!fixture.sandbox.root.join("executed").exists());
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn unknown_capability_is_a_preflight_failure_not_a_skip() {
    let fixture = Fixture::new();
    let (result, record) = fixture.library(
        &json!({"platforms":[],"capabilities":["undeclared-probe"]}),
        None,
    );
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(record["outcome"], "failed");
    assert!(
        record["evidence"]
            .as_str()
            .unwrap()
            .contains("unknown verifier capability")
    );
    assert!(!fixture.sandbox.root.join("executed").exists());
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn worker_accepts_exported_foreign_skip_without_claiming_coverage() {
    let fixture = Fixture::new();
    let result = fixture.gate(&[PASS, fixture.foreign()], true, "check");
    assert!(result.status.success(), "{result:?}");
    assert!(combined(&result).contains("unverified"));
    assert!(!fixture.sandbox.root.join(".loom/marker.json").exists());
    let records = records(
        fixture
            .sandbox
            .read(".loom/logs/gate/verifier-results.jsonl")
            .as_bytes(),
    );
    assert_ne!(records, Vec::<Value>::new());
    assert!(serde_json::to_string(&records).unwrap().contains("skipped"));
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn host_acceptance_retains_skip_exit_77() {
    let fixture = Fixture::new();
    let result = fixture.gate(&[PASS, fixture.foreign()], false, "check");
    assert_eq!(result.status.code(), Some(77), "{result:?}");
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn worker_failure_remains_blocking_alongside_authorized_skip() {
    let fixture = Fixture::new();
    let result = fixture.gate(&[fixture.foreign(), FAIL], true, "check");
    assert_eq!(result.status.code(), Some(1), "{result:?}");
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn worker_unreported_skip_remains_blocking() {
    let fixture = Fixture::new();
    let result = fixture.gate(&[UNEXPECTED], true, "check");
    assert_eq!(result.status.code(), Some(1), "{result:?}");
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn builder_non_darwin_skip_cannot_be_worker_acceptance() {
    let mut fixture = Fixture::new();
    fixture.builder("Linux", "26.0", 77);
    let result = fixture.gate(&[BUILDER], true, "system");
    fixture.assert_worker_builder_skip(&result);
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn builder_older_darwin_skip_cannot_be_worker_acceptance() {
    let mut fixture = Fixture::new();
    fixture.builder("Darwin", "25.6", 77);
    let result = fixture.gate(&[BUILDER], true, "system");
    fixture.assert_worker_builder_skip(&result);
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn builder_supported_darwin_unexpected_skip_blocks_worker() {
    let mut fixture = Fixture::new();
    fixture.builder("Darwin", "26.0", 77);
    let result = fixture.gate(&[BUILDER], true, "system");
    fixture.assert_worker_builder_skip(&result);
}
#[test]
#[ignore = "requires actual packaged verifier and consumed Loom"]
fn builder_skip_and_real_failure_remain_worker_blocking() {
    let mut fixture = Fixture::new();
    fixture.builder("Linux", "26.0", 77);
    let result = fixture.gate(&[BUILDER, FAIL], true, "system");
    fixture.assert_worker_builder_skip(&result);
}
