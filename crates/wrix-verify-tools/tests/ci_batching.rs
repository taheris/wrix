//! Actual test-ci batching, failure, interruption and consumed Loom skip contracts.
#![cfg(test)]
#[path = "common/verifier/mod.rs"]
mod fixture;
use fixture::{Sandbox, capture, combined, git_locals, quote, records, text};
use rustix::process::{Pid, Signal, kill_process_group};
use serde_json::{Value, json};
use std::{
    env, fs,
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Child, Output, Stdio},
    time::{Duration, Instant},
};

#[test]
fn quoted_executable_fixture_preserves_spaces_and_apostrophes_and_exit_status() {
    let sandbox = Sandbox::new("ci-executable-boundary-");
    let program = sandbox.root.join("path with ' apostrophe");
    fixture::executable(
        &program,
        "#!/usr/bin/env bash\nset -euo pipefail\nprintf '%s\\n' fixture-output\nprintf '%s\\n' fixture-error >&2\nexit 23\n",
    );
    let result = capture(sandbox.command("bash").args(["-c", &quote(&program)]));
    assert_eq!(result.status.code(), Some(23));
    assert_eq!(text(&result.stdout), "fixture-output\n");
    assert_eq!(text(&result.stderr), "fixture-error\n");
}

fn native_suite_command(
    sandbox: &Sandbox,
    cargo_status: i32,
    timeout_status: Option<i32>,
) -> std::process::Command {
    let bin = sandbox.root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fixture::executable(
        &bin.join("cargo"),
        &format!(
            "#!/usr/bin/env bash\nset -euo pipefail\ntest \"$NEXTEST_EXPERIMENTAL_SETUP_SCRIPTS\" = 1\nprintf '%s\\n' \"$@\" > {}\nexit {cargo_status}\n",
            quote(sandbox.root.join("cargo-args"))
        ),
    );
    if let Some(status) = timeout_status {
        fixture::executable(
            &bin.join("timeout"),
            &format!(
                "#!/usr/bin/env bash\nset -euo pipefail\nprintf '%s\\n' \"$@\" > {}\nexit {status}\n",
                quote(sandbox.root.join("timeout-args"))
            ),
        );
    }
    let path = sandbox
        .environment
        .get(std::ffi::OsStr::new("PATH"))
        .unwrap();
    let mut command = sandbox.command("bash");
    command
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bin/test-native"))
        .env(
            "PATH",
            format!("{}:{}", bin.display(), path.to_string_lossy()),
        );
    command
}

#[test]
fn native_suite_runs_all_targets_with_automatic_setup_enabled() {
    let sandbox = Sandbox::new("native-suite-complete-");
    let result = capture(&mut native_suite_command(&sandbox, 0, None));
    assert!(result.status.success(), "{}", combined(&result));
    assert_eq!(
        sandbox.read("cargo-args"),
        "nextest\nrun\n--workspace\n--all-targets\n"
    );
    assert!(text(&result.stdout).contains("including compilation and automatic setup"));
}

#[test]
fn native_suite_propagates_test_failure_without_reporting_success() {
    let sandbox = Sandbox::new("native-suite-failure-");
    let result = capture(&mut native_suite_command(&sandbox, 23, None));
    assert_eq!(result.status.code(), Some(23), "{}", combined(&result));
    assert!(!text(&result.stdout).contains("Native suite:"));
}

#[test]
fn native_suite_caps_the_entire_command_including_cancellation() {
    let sandbox = Sandbox::new("native-suite-deadline-");
    let result = capture(&mut native_suite_command(&sandbox, 0, Some(124)));
    assert_eq!(result.status.code(), Some(124), "{}", combined(&result));
    assert_eq!(
        sandbox.read("timeout-args"),
        "--signal=TERM\n--kill-after=5s\n595s\ncargo\nnextest\nrun\n--workspace\n--all-targets\n"
    );
    assert!(!sandbox.root.join("cargo-args").exists());
    assert!(!text(&result.stdout).contains("Native suite:"));
}

#[test]
fn native_suite_rejects_filters_instead_of_silently_omitting_tests() {
    let sandbox = Sandbox::new("native-suite-filter-");
    let result = capture(native_suite_command(&sandbox, 0, None).arg("--filter"));
    assert_eq!(result.status.code(), Some(64), "{}", combined(&result));
    assert!(!sandbox.root.join("cargo-args").exists());
}

const PASS: &str = "test-linux-builder-sshd-hardening";
const STATUS: &str = "test-linux-builder-image-source-kind";
const BROKEN: &str = "test-linux-builder-source-kind-load-transport";
const LIVE_BROKEN: &str = "test-security-execution-metadata-lifecycle";
const OFFLINE: &str = "test-container-pre-commit";
#[cfg(target_os = "linux")]
const VM: &str = "test-services-devshell-start-independent";
#[cfg(target_os = "linux")]
const VM_BROKEN: &str = "test-services-limit-mode-cache-endpoint";
fn input(name: &str) -> String {
    env::var(name).unwrap()
}
#[cfg(target_os = "linux")]
fn vm_requirements() -> Value {
    json!({"platforms":["aarch64-linux","x86_64-linux"],"capabilities":[]})
}
struct Fixture {
    sandbox: Sandbox,
    system: String,
}
impl Fixture {
    fn new() -> Self {
        let mut sandbox = Sandbox::new("wrix-ci-batching-");
        for name in git_locals() {
            sandbox.environment.remove(std::ffi::OsStr::new(&name));
        }
        sandbox
            .environment
            .insert("GIT_CONFIG_GLOBAL".into(), "/dev/null".into());
        sandbox
            .environment
            .insert("GIT_CONFIG_NOSYSTEM".into(), "1".into());
        let system = input("WRIX_TEST_CI_SYSTEM");
        let boundary = sandbox.root.file_name().unwrap().to_str().unwrap();
        let source = include_str!("fixtures/ci-batching.nix.in")
            .replace("@SYSTEM@", &system)
            .replace(
                "@BASH@",
                &serde_json::to_string(&format!("path:{}", input("WRIX_TEST_CI_BASH"))).unwrap(),
            )
            .replace(
                "@COREUTILS@",
                &serde_json::to_string(&format!("path:{}", input("WRIX_TEST_CI_COREUTILS")))
                    .unwrap(),
            )
            .replace("@BOUNDARY@", boundary);
        sandbox.write("flake.nix", &source);
        for args in [
            vec!["init", "-q"],
            vec!["add", "flake.nix"],
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
                "Create CI fixture",
            ],
        ] {
            let result = capture(sandbox.command("git").args(args).current_dir(&sandbox.root));
            assert!(result.status.success(), "{}", combined(&result));
        }
        Self { sandbox, system }
    }
    fn command(&self, program: &str) -> std::process::Command {
        let mut command = self.sandbox.command(program);
        command.current_dir(&self.sandbox.root).env(
            "NIX_CONFIG",
            format!(
                "{}\neval-cache = false\nmax-jobs = 1\ncores = 2\n",
                self.sandbox
                    .environment
                    .get(std::ffi::OsStr::new("NIX_CONFIG"))
                    .map_or_else(String::new, |v| v.to_string_lossy().into_owned())
            ),
        );
        command
    }
    fn lock_inputs(&self) {
        let lock = capture(self.command("nix").args(["flake", "lock"]));
        assert!(lock.status.success(), "{}", combined(&lock));
        for args in [
            vec!["add", "flake.lock"],
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
                "Lock fixture inputs",
            ],
        ] {
            let result = capture(self.command("git").args(args));
            assert!(result.status.success(), "{}", combined(&result));
        }
    }
    fn cached_command(&self, program: &str) -> std::process::Command {
        let mut command = self.command(program);
        let config = command
            .get_envs()
            .find_map(|(name, value)| {
                (name == "NIX_CONFIG").then(|| value.unwrap().to_string_lossy().into_owned())
            })
            .unwrap();
        command.env("NIX_CONFIG", format!("{config}\neval-cache = true\n"));
        command
    }
    fn run_with(
        &self,
        apps: &[&str],
        status: &str,
        git_dir: bool,
    ) -> (Output, Vec<Value>, Vec<String>) {
        self.sandbox.write("calls", "");
        let mut command = self.command(&input("WRIX_TEST_CI_RUNNER"));
        command
            .args(["--json"])
            .args(apps)
            .env("WRIX_TEST_CI_STATUS", status)
            .env("WRIX_TEST_CI_CALLS", self.sandbox.root.join("calls"));
        if git_dir {
            command.env("GIT_DIR", self.sandbox.root.join("not-a-repository"));
        }
        let result = capture(&mut command);
        let verdicts = records(&result.stdout);
        assert_eq!(
            verdicts
                .iter()
                .map(|v| v["target"].as_str().unwrap())
                .collect::<Vec<_>>(),
            apps,
            "{}",
            combined(&result)
        );
        let calls = self
            .sandbox
            .read("calls")
            .lines()
            .map(str::to_owned)
            .collect();
        (result, verdicts, calls)
    }
    fn run(&self, apps: &[&str], status: &str) -> (Output, Vec<Value>, Vec<String>) {
        self.run_with(apps, status, false)
    }
    fn probe(&self, requirements: &Value) -> Output {
        let result = capture(self.command("bash").args([
            "-c",
            "source \"$1\"; verifier_preflight \"$2\" \"$3\"",
            "fixture",
            &input("WRIX_TEST_VERIFIER_LIBRARY"),
            &self.system,
            &requirements.to_string(),
        ]));
        assert!(
            matches!(result.status.code(), Some(0 | 77)),
            "{}",
            combined(&result)
        );
        result
    }
    fn not_built(&self, app: &str) {
        let result = capture(self.command("nix").args([
            "eval",
            "--raw",
            &format!(".#legacyPackages.{}.ciApps.{app}.drvPath", self.system),
        ]));
        assert!(result.status.success(), "{}", combined(&result));
        let log = capture(
            self.command("nix")
                .args(["log", text(&result.stdout).trim()]),
        );
        assert!(!combined(&log).contains("BUILD_BOUNDARY"));
    }
    #[cfg(target_os = "linux")]
    fn gate(&self, apps: &[&str], worker: bool, status: &str) -> Output {
        use std::fmt::Write as _;
        let mut spec = "# Fixture\n\n## Success Criteria\n\n".to_owned();
        for (index, app) in apps.iter().enumerate() {
            writeln!(spec, "- Target {index} [check](test-ci:{app})").unwrap();
        }
        self.sandbox.write("specs/fixture.md", &spec);
        self.sandbox.write("loom.toml", &format!("[runner.check.test-ci]\nmatch = '^test-ci:(.+)$'\ncommand = {}\ntarget = '{{capture_1}}'\njoin = ' '\nparse = 'json-lines'\ncwd = '.'\nskip_policy = 'sandbox-capability'\nskip_capabilities = ['virtiofsd-capabilities']\n", serde_json::to_string(&format!("{} --json {{targets}}", quote(input("WRIX_TEST_CI_RUNNER")))).unwrap()));
        for args in [
            vec!["add", "specs", "loom.toml"],
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
                "--allow-empty",
                "-qm",
                "Update gate fixture",
            ],
        ] {
            let result = capture(self.command("git").args(args));
            assert!(result.status.success(), "{}", combined(&result));
        }
        let mut command = self.command(&input("WRIX_TEST_CI_LOOM"));
        command
            .args(["gate", "verify", "--workspace"])
            .arg(&self.sandbox.root)
            .arg("--tree")
            .env("WRIX_TEST_CI_STATUS", status)
            .env("WRIX_TEST_CI_CALLS", self.sandbox.root.join("calls"))
            .env_remove("LOOM_INSIDE");
        if worker {
            command.env("LOOM_INSIDE", "1");
        }
        capture(&mut command)
    }
}

#[test]
#[ignore = "owning test-ci boundary"]
fn image_scopes_retain_only_genuine_rust_runtime_dependencies() {
    let fixture = Fixture::new();
    let runtime = [
        "test-image-install-real-skopeo",
        "test-image-install-digest-skip",
        "test-wrix-images-source-kind",
        "test-wrix-image-labels",
    ];
    let image = [
        "test-image-tier-graph",
        "test-shared-nix-image-contract",
        "test-loom-agent-images",
        "test-playwright-chromium-closure",
        "test-playwright-mandatory-flags",
    ];
    let result = capture(
        fixture
            .command(&input("WRIX_TEST_CI_RUNNER"))
            .arg("--print-inputs")
            .args(runtime)
            .args(image),
    );
    assert!(result.status.success(), "{}", combined(&result));
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    for target in runtime.into_iter().chain(image) {
        let values = value["inputs"][target].as_array().unwrap();
        for name in ["/**/*.nix", "/specs/"] {
            assert!(!values.contains(&json!(name)), "{target}");
        }
        for name in ["/modules/flake/", "/flake.lock"] {
            assert!(values.contains(&json!(name)), "{target}");
        }
        assert_eq!(
            values.contains(&json!("/crates/")),
            runtime.contains(&target)
        );
    }
    assert!(
        value["inputs"]["test-playwright-chromium-closure"]
            .as_array()
            .unwrap()
            .contains(&json!("/lib/sandbox/"))
    );
    assert!(
        !value["inputs"]["test-playwright-mandatory-flags"]
            .as_array()
            .unwrap()
            .contains(&json!("/lib/sandbox/"))
    );
}
fn refused(argument: &str) {
    let f = Fixture::new();
    let r = capture(f.command(&input("WRIX_TEST_CI_RUNNER")).arg(argument));
    assert_eq!(r.status.code(), Some(64));
    assert!(text(&r.stderr).contains("--json"));
    assert!(!text(&r.stderr).contains("WRIX-CI-BATCH-QUERY"));
    assert_eq!(r.stdout, [] as [u8; 0]);
}
#[test]
#[ignore = "owning test-ci boundary"]
fn positional_app_names_require_json_without_running_the_full_suite() {
    refused(PASS);
}
#[test]
#[ignore = "owning test-ci boundary"]
fn unknown_options_do_not_trigger_the_full_suite() {
    refused("--typo");
}
#[test]
#[ignore = "owning test-ci boundary"]
fn selected_runners_batch_before_independent_execution() {
    let f = Fixture::new();
    let (r, v, c) = f.run(&[PASS, STATUS, PASS], "pass");
    assert_eq!(r.status.code(), Some(0), "{}", combined(&r));
    assert!(v.iter().all(|v| v["outcome"] == "passed"));
    assert_eq!(c, [PASS, STATUS, PASS]);
    assert!(text(&r.stderr).contains("trace: WRIX-CI-BATCH-QUERY"));
}
#[test]
#[ignore = "owning test-ci boundary"]
fn admitted_metadata_reuses_the_locked_flake_evaluation_cache() {
    let f = Fixture::new();
    f.lock_inputs();
    let targets = [PASS, STATUS].map(|app| format!(".#legacyPackages.{}.ciApps.{app}", f.system));
    let seeded = capture(
        f.cached_command("nix")
            .args(["build", "--dry-run", "--json"])
            .args(&targets),
    );
    assert!(seeded.status.success(), "{}", combined(&seeded));
    assert!(text(&seeded.stderr).contains("WRIX-CI-BATCH-QUERY"));
    let result = capture(
        f.cached_command(&input("WRIX_TEST_CI_RUNNER"))
            .args(["--json", PASS, STATUS])
            .env("WRIX_TEST_CI_STATUS", "pass")
            .env("WRIX_TEST_CI_CALLS", f.sandbox.root.join("calls")),
    );
    assert!(result.status.success(), "{}", combined(&result));
    assert!(
        records(&result.stdout)
            .iter()
            .all(|record| record["outcome"] == "passed")
    );
    assert!(
        !text(&result.stderr).contains("WRIX-CI-BATCH-QUERY"),
        "{}",
        combined(&result)
    );
}

#[test]
#[ignore = "owning test-ci boundary"]
fn completed_window_executes_before_later_image_producers_are_materialized() {
    let f = Fixture::new();
    f.lock_inputs();
    let stream = fs::File::create(f.sandbox.root.join("ordered-output")).unwrap();
    let status = f
        .cached_command(&input("WRIX_TEST_CI_RUNNER"))
        .args(["--json", PASS, STATUS, OFFLINE])
        .env("WRIX_TEST_CI_STATUS", "pass")
        .env("WRIX_TEST_CI_CALLS", f.sandbox.root.join("calls"))
        .stdout(Stdio::from(stream.try_clone().unwrap()))
        .stderr(Stdio::from(stream))
        .status()
        .unwrap();
    let diagnostic = f.sandbox.read("ordered-output");
    assert!(status.success(), "{diagnostic}");
    assert_eq!(
        f.sandbox.read("calls"),
        format!("{PASS}\n{STATUS}\n{OFFLINE}\n")
    );
    let execution = diagnostic
        .lines()
        .position(|line| {
            serde_json::from_str::<Value>(line)
                .is_ok_and(|verdict| verdict["target"] == STATUS && verdict["outcome"] == "passed")
        })
        .unwrap();
    let next_producer = diagnostic
        .lines()
        .position(|line| {
            line.starts_with("building '") && line.contains(&format!("-{OFFLINE}.drv"))
        })
        .unwrap();
    assert!(execution < next_producer, "{diagnostic}");
    assert_eq!(diagnostic.matches("trace: WRIX-CI-BATCH-QUERY").count(), 1);
}

#[test]
#[ignore = "owning test-ci boundary"]
fn checked_results_survive_window_rotation_and_are_removed_on_exit() {
    let f = Fixture::new();
    let parent = f.sandbox.root.join("batch-parent");
    fs::create_dir(&parent).unwrap();
    let result = capture(
        f.command(&input("WRIX_TEST_CI_RUNNER"))
            .args(["--json", PASS, STATUS, OFFLINE])
            .env("TMPDIR", &parent)
            .env("WRIX_TEST_CI_BATCH_PARENT", &parent)
            .env(
                "WRIX_TEST_CI_RESULT_ROOTS",
                f.sandbox.root.join("result-roots"),
            )
            .env("WRIX_TEST_CI_STATUS", "pass")
            .env("WRIX_TEST_CI_CALLS", f.sandbox.root.join("calls")),
    );
    assert!(result.status.success(), "{}", combined(&result));
    let roots = f.sandbox.read("result-roots");
    assert!(roots.contains(&format!("checked-{PASS}")));
    assert!(roots.lines().any(|path| path.ends_with("/dispatcher")));
    for root in roots.lines() {
        assert!(!std::path::Path::new(root).exists());
    }
}

#[test]
#[ignore = "owning test-ci boundary"]
fn fixture_execution_is_independent_of_caller_git_context() {
    let f = Fixture::new();
    let (r, v, c) = f.run_with(&[PASS, STATUS], "pass", true);
    assert_eq!(r.status.code(), Some(0), "{}", combined(&r));
    assert!(v.iter().all(|v| v["outcome"] == "passed"));
    assert_eq!(c, [PASS, STATUS]);
}
#[test]
#[ignore = "owning test-ci boundary"]
fn script_failure_keeps_other_app_verdicts() {
    let f = Fixture::new();
    let (r, v, c) = f.run(&[PASS, STATUS], "fail");
    assert_eq!(r.status.code(), Some(1));
    assert_eq!(
        v.iter().map(|v| &v["outcome"]).collect::<Vec<_>>(),
        [&json!("passed"), &json!("failed")]
    );
    assert!(
        v[1]["evidence"]
            .as_str()
            .unwrap()
            .contains("fixture-failure")
    );
    assert_eq!(c, [PASS, STATUS]);
}
#[test]
#[ignore = "owning test-ci boundary"]
fn unreported_exit_77_retains_skip_without_policy_authorization() {
    let f = Fixture::new();
    let (r, v, _) = f.run(&[PASS, STATUS], "skip");
    assert_eq!(r.status.code(), Some(77), "{}", combined(&r));
    assert_eq!(v[0]["outcome"], "passed");
    assert_eq!(v[1]["pass"], false);
    assert_eq!(v[1]["skipped"], true);
    assert!(v[1].get("skip_reason").is_none());
    assert_eq!(v[1]["execution"]["capabilities"], json!([]));
    assert!(v[1]["evidence"].as_str().unwrap().contains("fixture-skip"));
}
#[test]
#[ignore = "owning test-ci boundary"]
fn build_failure_dominates_skip_and_keeps_individual_results() {
    let f = Fixture::new();
    let (r, v, c) = f.run(&[PASS, STATUS, BROKEN], "skip");
    assert_eq!(r.status.code(), Some(1));
    assert_eq!(v[0]["outcome"], "passed");
    assert_eq!(v[1]["skipped"], true);
    assert_eq!(v[2]["outcome"], "failed");
    assert_eq!(c, [PASS, STATUS]);
}
#[test]
#[ignore = "owning test-ci boundary"]
fn all_unreported_skips_exit_77() {
    let f = Fixture::new();
    let (r, v, c) = f.run(&[STATUS, STATUS], "skip");
    assert_eq!(r.status.code(), Some(77));
    assert!(v.iter().all(|v| v["skipped"] == true));
    assert_eq!(c, [STATUS, STATUS]);
}
#[test]
#[ignore = "owning test-ci boundary"]
fn offline_hook_executes_without_host_networking_capability() {
    let f = Fixture::new();
    let (r, v, c) = f.run(&[PASS, OFFLINE], "pass");
    assert_eq!(r.status.code(), Some(0), "{}", combined(&r));
    assert!(v.iter().all(|v| v["outcome"] == "passed"));
    assert_eq!(v[1]["execution"]["capabilities"], json!([]));
    assert_eq!(c, [PASS, OFFLINE]);
}
#[test]
#[ignore = "owning test-ci boundary"]
fn unavailable_live_app_is_not_built_before_a_classified_skip() {
    let f = Fixture::new();
    let probe = f.probe(&json!({"platforms":["aarch64-darwin","x86_64-darwin","aarch64-linux","x86_64-linux"],"capabilities":["container-runtime"]}));
    for apps in [vec![LIVE_BROKEN], vec![PASS, STATUS, LIVE_BROKEN]] {
        let (r, v, c) = f.run(&apps, "pass");
        assert_eq!(c, apps[..apps.len() - 1]);
        let last = v.last().unwrap();
        if probe.status.code() == Some(77) {
            assert_eq!(r.status.code(), Some(77), "{}", combined(&r));
            assert_eq!(last["outcome"], "skipped");
            assert!(last.get("skip_reason").is_some());
            f.not_built(LIVE_BROKEN);
        } else {
            assert_eq!(r.status.code(), Some(1), "{}", combined(&r));
            assert_eq!(last["outcome"], "failed");
            assert!(
                last["evidence"]
                    .as_str()
                    .unwrap()
                    .contains("CI app build failed")
            );
        }
    }
}
#[test]
#[ignore = "owning test-ci boundary"]
fn failed_batch_falls_back_to_individual_build_verdicts() {
    let f = Fixture::new();
    let (r, v, c) = f.run(&[PASS, BROKEN], "pass");
    assert_eq!(r.status.code(), Some(1));
    assert_eq!(v[0]["outcome"], "passed");
    assert_eq!(v[1]["outcome"], "failed");
    assert_ne!(v[1]["evidence"].as_str().unwrap(), "");
    assert_eq!(c, [PASS]);
}
#[test]
#[ignore = "owning test-ci boundary"]
fn unknown_apps_fail_without_suppressing_valid_apps() {
    let f = Fixture::new();
    let (r, v, c) = f.run(&[PASS, "not-a-ci-app", STATUS], "pass");
    assert_eq!(r.status.code(), Some(1));
    assert_eq!(v[0]["outcome"], "passed");
    assert_eq!(v[1]["outcome"], "failed");
    assert_eq!(v[2]["outcome"], "passed");
    assert_eq!(v[1]["evidence"], "unknown test-ci app");
    assert_eq!(c, [PASS, STATUS]);
}
#[test]
#[ignore = "owning test-ci boundary"]
fn single_app_retains_individual_build_execution() {
    let f = Fixture::new();
    let (r, v, c) = f.run(&[PASS], "pass");
    assert_eq!(r.status.code(), Some(0), "{}", combined(&r));
    assert_eq!(v[0]["outcome"], "passed");
    assert_eq!(c, [PASS]);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "owning Linux VM admission boundary"]
fn vm_prerequisite_precedes_execution_with_matching_metadata() {
    let f = Fixture::new();
    let probe = f.probe(&vm_requirements());
    let (r, v, c) = f.run(&[VM, VM], "pass");
    assert_eq!(r.status.code(), probe.status.code(), "{}", combined(&r));
    assert_eq!(
        c,
        if probe.status.success() {
            vec![VM, VM]
        } else {
            vec![]
        }
    );
    let mut execution = vm_requirements();
    execution["platform"] = f.system.into();
    for v in v {
        assert_eq!(v["execution"], execution);
        assert_eq!(
            v["outcome"],
            if probe.status.success() {
                "passed"
            } else {
                "skipped"
            }
        );
        if !probe.status.success() {
            assert_eq!(
                v["skip_reason"],
                serde_json::from_slice::<Value>(&probe.stdout).unwrap()
            );
        }
    }
}
#[cfg(target_os = "linux")]
#[test]
#[ignore = "owning Linux VM admission boundary"]
fn vm_capability_admission_precedes_batch_and_single_runner_builds() {
    let f = Fixture::new();
    let probe = f.probe(&vm_requirements());
    for apps in [vec![VM_BROKEN], vec![VM, VM_BROKEN]] {
        let (r, v, _) = f.run(&apps, "pass");
        let mut execution = vm_requirements();
        execution["platform"] = f.system.clone().into();
        let last = v.last().unwrap();
        assert_eq!(last["execution"], execution);
        if probe.status.code() == Some(77) {
            assert_eq!(r.status.code(), Some(77), "{}", combined(&r));
            assert!(v.iter().all(|v| v["outcome"] == "skipped"));
            f.not_built(VM_BROKEN);
        } else {
            assert_eq!(probe.status.code(), Some(0));
            assert_eq!(r.status.code(), Some(1), "{}", combined(&r));
            assert_eq!(last["outcome"], "failed");
            assert!(
                last["evidence"]
                    .as_str()
                    .unwrap()
                    .contains("CI app build failed")
            );
        }
    }
}
#[cfg(target_os = "linux")]
#[test]
#[ignore = "owning Linux consumed Loom boundary"]
fn worker_accepts_vm_capability_gap_without_claiming_coverage() {
    let f = Fixture::new();
    let r = f.gate(&[VM], true, "pass");
    assert_eq!(r.status.code(), Some(0), "{}", combined(&r));
    let (probe, v, _) = f.run(&[VM], "pass");
    if probe.status.code() == Some(77) {
        assert!(combined(&r).contains("unverified"));
        assert_eq!(v[0]["outcome"], "skipped");
        assert!(!f.sandbox.root.join(".loom/marker.json").exists());
    }
}
#[cfg(target_os = "linux")]
#[test]
#[ignore = "owning Linux consumed Loom boundary"]
fn host_vm_capability_skip_retains_exit_77() {
    let f = Fixture::new();
    let (probe, _, _) = f.run(&[VM], "pass");
    let r = f.gate(&[VM], false, "pass");
    assert_eq!(r.status.code(), probe.status.code(), "{}", combined(&r));
}
#[cfg(target_os = "linux")]
#[test]
#[ignore = "owning Linux consumed Loom boundary"]
fn worker_vm_skip_cannot_mask_a_runtime_failure() {
    let f = Fixture::new();
    let r = f.gate(&[VM, STATUS], true, "fail");
    assert_eq!(r.status.code(), Some(1), "{}", combined(&r));
}

struct Owned(Child);
impl Drop for Owned {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            let pid = Pid::from_raw(i32::try_from(self.0.id()).unwrap()).unwrap();
            match kill_process_group(pid, Signal::KILL) {
                Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                Err(error) => panic!("cannot stop owned CI fixture: {error}"),
            }
            self.0.wait().unwrap();
        }
    }
}
#[test]
#[ignore = "owning test-ci interruption boundary"]
fn terminated_batch_removes_its_temporary_gc_roots() {
    let f = Fixture::new();
    let tmp = tempfile::Builder::new()
        .prefix("ci-termination-")
        .tempdir_in(&f.sandbox.root)
        .unwrap();
    let ready = tmp.path().join("ready");
    let stdout = tempfile::tempfile().unwrap();
    let stderr_path = tmp.path().join("stderr");
    let stderr = fs::File::create(&stderr_path).unwrap();
    let mut command = f.command(&input("WRIX_TEST_CI_RUNNER"));
    command
        .args(["--json", PASS, STATUS])
        .env("TMPDIR", tmp.path())
        .env("WRIX_TEST_CI_STATUS", "wait")
        .env("WRIX_TEST_CI_CALLS", tmp.path().join("calls"))
        .env("WRIX_TEST_CI_READY", &ready)
        .process_group(0)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    let mut child = Owned(command.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(60);
    while !ready.exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "fixture exited before interruption"
        );
        assert!(Instant::now() < deadline, "fixture never became ready");
        std::thread::sleep(Duration::from_millis(50));
    }
    let roots = fs::read_dir(tmp.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("wrix-ci-runners.")
        })
        .collect::<Vec<PathBuf>>();
    assert_eq!(roots.len(), 1);
    assert!(
        roots[0].join("runner").is_symlink(),
        "{}",
        fs::read_to_string(&stderr_path).unwrap()
    );
    kill_process_group(
        Pid::from_raw(i32::try_from(child.0.id()).unwrap()).unwrap(),
        Signal::TERM,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "interrupted fixture did not exit within 10s"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.code(), Some(143));
    assert!(
        !roots[0].exists(),
        "interrupted batch retained image GC roots"
    );
}
