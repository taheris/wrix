//! Definition-derived input discovery through the actual dispatcher and Loom.
#![cfg(test)]
#[path = "common/verifier/mod.rs"]
mod fixture;

use fixture::{Sandbox, capture, combined, executable, git_locals, quote, records, text};
use serde_json::{Value, json};
use std::{collections::BTreeSet, env, fmt::Write as _, fs, path::PathBuf, process::Output};

const KNOWN: &str = "devshell.no-prek-install";
const OPAQUE: &str = "notifications.focus-target-envelope";
const CI_KNOWN: &str = "test-linux-builder-source-kind-load-transport";
const CI_OPAQUE: &str = "test-security-execution-metadata-lifecycle";
fn binary(name: &str) -> PathBuf {
    env::var_os(name).unwrap().into()
}
fn keys(value: &Value) -> BTreeSet<&str> {
    value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect()
}
fn set<'a>(values: impl IntoIterator<Item = &'a str>) -> BTreeSet<&'a str> {
    values.into_iter().collect()
}
fn paths_contain(value: &Value, expected: &str) -> bool {
    value.as_array().unwrap().contains(&json!(expected))
}

struct Fixture {
    sandbox: Sandbox,
    system: String,
}
impl Fixture {
    fn new() -> Self {
        let mut sandbox = Sandbox::new("wrix-verifier-inputs-");
        for name in git_locals() {
            sandbox.environment.remove(std::ffi::OsStr::new(&name));
        }
        for (key, value) in [
            ("LOOM_INSIDE", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
        ] {
            sandbox.environment.insert(key.into(), value.into());
        }
        Self {
            sandbox,
            system: env::var("WRIX_TEST_SYSTEM").unwrap(),
        }
    }
    fn command(&self, program: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> Output {
        capture(
            self.sandbox
                .command(program)
                .current_dir(&self.sandbox.root)
                .args(args),
        )
    }
    fn query(&self, program: &str, targets: &[&str]) -> Value {
        let result = capture(
            self.sandbox
                .command(binary(program))
                .current_dir(&self.sandbox.root)
                .arg("--print-inputs")
                .args(targets),
        );
        assert!(result.status.success(), "{result:?}");
        let document: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(keys(&document), set(["inputs"]));
        assert!(document["inputs"].is_object());
        assert_eq!(self.sandbox.read("calls"), "");
        document["inputs"].clone()
    }
    fn fixture(&self, malformed: bool, system: bool) {
        let source = binary("WRIX_TEST_SOURCE");
        for relative in [
            "tests/verify/profiles-eval.nix",
            "tests/verify/agent-defaults.nix",
            "tests/verify/network-rules.nix",
            "tests/lib/inputs.nix",
            "tests/lib/browser-resources.nix",
        ] {
            let destination = self.sandbox.root.join(relative);
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::copy(source.join(relative), destination).unwrap();
        }
        self.sandbox.write(
            "lib/devshell/default.nix",
            "# source fixture with no forbidden hook mutation\n",
        );
        self.sandbox.write(
            "flake.nix",
            &format!(
                "{{\ninputs.nixpkgs.url = {};\noutputs = {{ ... }}: {{ }};\n}}\n",
                serde_json::to_string(&format!("path:{}", binary("WRIX_TEST_NIXPKGS").display()))
                    .unwrap()
            ),
        );
        self.sandbox.write(
            "tests/standalone/notify-test.sh",
            "#!/usr/bin/env bash\nset -euo pipefail\nprintf 'opaque\\n' >>\"$REPO_ROOT/calls\"\n",
        );
        let wrapper = self.sandbox.root.join("runner");
        let malformed_body = if malformed {
            "printf '%s\\n' '{\"inputs\":{\"devshell.no-prek-install\":\"malformed\"}}'; exit 0"
        } else {
            ":"
        };
        executable(
            &wrapper,
            &format!(
                r#"#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" == --print-inputs ]]; then
  printf 'query\n' >>"$REPO_ROOT/calls"
  {malformed_body}
else
  printf 'execute %s\n' "$*" >>"$REPO_ROOT/calls"
fi
exec {} "$@"
"#,
                quote(binary("WRIX_TEST_VERIFY"))
            ),
        );
        let command = quote(&wrapper);
        let mut config = format!(
            "[runner.check.verify]\nmatch = '^verify:(.+)$'\ncommand = {}\ninputs = {}\ntarget = '{{capture_1}}'\njoin = ' '\nparse = 'json-lines'\ncwd = '.'\n",
            serde_json::to_string(&format!("{command} {{targets}}")).unwrap(),
            serde_json::to_string(&format!("{command} {{print_inputs}} {{targets}}")).unwrap()
        );
        if system {
            write!(
                config,
                "[runner.system.verify]\nmatch = '^verify:(.+)$'\ncommand = {}\ntarget = '{{capture_1}}'\njoin = ' '\nparse = 'json-lines'\ncwd = '.'\n",
                serde_json::to_string(&format!("{command} {{targets}}")).unwrap()
            ).unwrap();
        }
        self.sandbox.write("loom.toml", &config);
        self.sandbox.write(".pre-commit-config.yaml", &json!({"repos":[{"repo":"local","hooks":[{"id":"fixture","name":"fixture","entry":"true","language":"system","always_run":true,"pass_filenames":false,"stages":["pre-commit"]}]}]}).to_string());
        let mut annotations = format!(
            "- Source hook assertion [check](verify:{KNOWN})\n- Independent opaque assertion [check](verify:{OPAQUE})\n"
        );
        if system {
            annotations += &format!("- System assertion [system](verify:{KNOWN})\n").repeat(2);
        }
        self.sandbox.write(
            "specs/fixture.md",
            &format!("# Fixture\n\n## Success Criteria\n\n{annotations}"),
        );
        self.sandbox.write("README.md", "Fixture documentation\n");
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
                "Create input fixture",
            ],
        ] {
            let result = self.command("git", &args);
            assert!(result.status.success(), "{result:?}");
        }
    }
    fn gate(&self, path: &str, tier: &str, commit: bool) -> (Output, Vec<String>) {
        // Preserve the actual pre-push range path, not the upstream --files bug.
        if commit {
            let mut contents = self.sandbox.read(path);
            contents.push('\n');
            self.sandbox.write(path, &contents);
            for args in [
                vec!["add", path],
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
                    "Change fixture subject",
                ],
            ] {
                let result = self.command("git", &args);
                assert!(result.status.success(), "{result:?}");
            }
        }
        self.sandbox.write("calls", "");
        let result = capture(
            self.sandbox
                .command(binary("WRIX_TEST_LOOM"))
                .current_dir(&self.sandbox.root)
                .args(["gate", tier, "--workspace"])
                .arg(&self.sandbox.root)
                .args(["--diff", "HEAD^..HEAD"]),
        );
        let calls = self
            .sandbox
            .read("calls")
            .lines()
            .map(str::to_owned)
            .collect();
        (result, calls)
    }
}

#[test]
#[ignore = "requires actual verifier/CI/Loom and native Nix input providers"]
fn verify_batch_normalizes_ids_and_omits_opaque_definitions() {
    let fixture = Fixture::new();
    let inputs = fixture.query(
        "WRIX_TEST_VERIFY",
        &[&format!("verify:{KNOWN}"), KNOWN, OPAQUE],
    );
    assert_eq!(keys(&inputs), set([KNOWN]));
    assert!(paths_contain(&inputs[KNOWN], "/lib/devshell/default.nix"));
}
#[test]
#[ignore = "requires actual verifier/CI/Loom and native Nix input providers"]
fn ci_batch_projects_recipe_resources_without_realizing_apps() {
    let fixture = Fixture::new();
    let inputs = fixture.query("WRIX_TEST_CI", &[CI_KNOWN, CI_OPAQUE, CI_KNOWN]);
    assert_eq!(keys(&inputs), set([CI_KNOWN]));
    assert!(paths_contain(&inputs[CI_KNOWN], "/lib/"));
    assert!(paths_contain(&inputs[CI_KNOWN], "/tests/builder/"));
    assert!(!fixture.sandbox.root.join("flake.nix").exists());
}
#[test]
#[ignore = "requires actual verifier/CI/Loom and native Nix input providers"]
fn ci_inventory_and_discovery_include_foreign_platform_targets() {
    let fixture = Fixture::new();
    let targets: Vec<_> = [
        "apple",
        "krun",
        "podman",
        "public-podman",
        "public-krun",
        "host-owner-linux",
        "interruption-linux",
        "pool-export-linux",
    ]
    .into_iter()
    .map(|runtime| format!("test-shared-nix-{runtime}"))
    .collect();
    let inventory = fixture.command(binary("WRIX_TEST_CI"), &["--list"]);
    assert!(inventory.status.success(), "{inventory:?}");
    for target in &targets {
        assert!(
            text(&inventory.stdout)
                .lines()
                .any(|line| line == format!("app {target}")),
            "{inventory:?}"
        );
    }
    assert_eq!(
        fixture.query(
            "WRIX_TEST_CI",
            &targets.iter().map(String::as_str).collect::<Vec<_>>()
        ),
        json!({})
    );
}
#[test]
#[ignore = "requires actual verifier/CI/Loom and native Nix input providers"]
fn ci_foreign_platform_target_skips_before_runner_realization() {
    let fixture = Fixture::new();
    let target = if fixture.system.ends_with("-linux") {
        "test-shared-nix-apple"
    } else {
        "test-shared-nix-public-podman"
    };
    let result = fixture.command(binary("WRIX_TEST_CI"), &["--json", target]);
    assert_eq!(result.status.code(), Some(77), "{result:?}");
    let record: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(record["outcome"], "skipped");
    assert!(!fixture.sandbox.root.join("flake.nix").exists());
}
#[test]
#[ignore = "requires actual verifier/CI/Loom and native Nix input providers"]
fn foreign_ci_packages_can_be_evaluated_without_native_platform_resources() {
    let fixture = Fixture::new();
    let targets = if fixture.system.ends_with("-linux") {
        vec!["test-shared-nix-apple"]
    } else {
        vec![
            "test-shared-nix-host-owner-linux",
            "test-shared-nix-interruption-linux",
            "test-shared-nix-pool-export-linux",
            "test-shared-nix-public-podman",
            "test-shared-nix-public-krun",
        ]
    };
    for target in targets {
        let result = fixture.command(
            "nix",
            &[
                "eval",
                "--no-warn-dirty",
                "--raw",
                &format!(
                    "{}#legacyPackages.{}.ciApps.{target}.drvPath",
                    binary("WRIX_TEST_SOURCE").display(),
                    fixture.system
                ),
            ],
        );
        assert!(result.status.success(), "{result:?}");
        let value = text(&result.stdout);
        assert!(value.starts_with("/nix/store/"), "{value}");
        assert_eq!(
            std::path::Path::new(&value).extension(),
            Some(std::ffi::OsStr::new("drv")),
            "{value}"
        );
    }
}
#[test]
#[ignore = "requires actual verifier/CI/Loom and native Nix input providers"]
fn single_opaque_target_stays_unknown_instead_of_empty_known() {
    let fixture = Fixture::new();
    for (program, target) in [("WRIX_TEST_VERIFY", OPAQUE), ("WRIX_TEST_CI", CI_OPAQUE)] {
        assert_eq!(fixture.query(program, &[target]), json!({}));
    }
}
#[test]
#[ignore = "requires actual verifier/CI/Loom and native Nix input providers"]
fn no_target_query_returns_only_definition_derived_known_entries() {
    let fixture = Fixture::new();
    for (program, target) in [("WRIX_TEST_VERIFY", KNOWN), ("WRIX_TEST_CI", CI_KNOWN)] {
        let inputs = fixture.query(program, &[]);
        assert!(inputs.get(target).is_some());
        assert!(paths_contain(&inputs[target], "/**/*.nix"));
        for paths in inputs.as_object().unwrap().values() {
            assert!(paths.as_array().unwrap().iter().all(Value::is_string));
            assert!(paths_contain(paths, "/flake.lock"));
            assert!(paths_contain(paths, "/tests/lib/verifier.sh"));
        }
    }
}
#[test]
#[ignore = "requires actual verifier/CI/Loom and native Nix input providers"]
fn image_recipes_retain_scoped_nix_inputs_without_a_blanket_repo_glob() {
    let fixture = Fixture::new();
    let targets = ["test-wrix-images-source-kind", "test-image-agent-marker"];
    let inputs = fixture.query("WRIX_TEST_CI", &targets);
    assert_eq!(keys(&inputs), set(targets));
    for paths in inputs.as_object().unwrap().values() {
        assert!(!paths_contain(paths, "/**/*.nix"));
        for path in [
            "/lib/sandbox/",
            "/modules/flake/",
            "/tests/default.nix",
            "/specs/image-builder.md",
        ] {
            assert!(paths_contain(paths, path));
        }
    }
    assert!(!paths_contain(
        &inputs["test-image-agent-marker"],
        "/crates/"
    ));
    assert!(paths_contain(
        &inputs["test-wrix-images-source-kind"],
        "/crates/"
    ));
    assert!(paths_contain(
        &inputs["test-wrix-images-source-kind"],
        "/lib/services/"
    ));
}
#[test]
#[ignore = "requires actual verifier/CI/Loom and native Nix input providers"]
fn unknown_id_rejects_whole_description_before_output() {
    let fixture = Fixture::new();
    for (program, target) in [("WRIX_TEST_VERIFY", KNOWN), ("WRIX_TEST_CI", CI_KNOWN)] {
        let result = fixture.command(
            binary(program),
            &["--print-inputs", target, "not-registered"],
        );
        assert_eq!(result.status.code(), Some(64), "{result:?}");
        assert_eq!(result.stdout, b"");
        assert!(text(&result.stderr).contains("not-registered"));
    }
}
#[test]
#[ignore = "requires actual verifier/CI/Loom and native Nix input providers"]
fn independent_change_runs_opaque_and_queries_group_once_per_session() {
    let fixture = Fixture::new();
    fixture.fixture(false, false);
    for index in 0..2 {
        let (result, calls) = fixture.gate("README.md", "verify", index == 0);
        assert!(result.status.success(), "{result:?}");
        assert_eq!(
            calls.iter().filter(|call| *call == "query").count(),
            1,
            "{calls:?}"
        );
        assert!(calls.contains(&format!("execute {OPAQUE}")));
        assert!(!calls.contains(&format!("execute {KNOWN}")));
        assert_eq!(calls.iter().filter(|call| *call == "opaque").count(), 1);
    }
}
#[test]
#[ignore = "requires actual verifier/CI/Loom and native Nix input providers"]
fn changed_source_selects_real_assertion_and_retains_failure() {
    let fixture = Fixture::new();
    fixture.fixture(false, false);
    fixture
        .sandbox
        .write("lib/devshell/default.nix", "\"prek install\"\n");
    let (result, calls) = fixture.gate("lib/devshell/default.nix", "verify", true);
    assert_eq!(result.status.code(), Some(1), "{result:?}");
    assert_eq!(
        calls.iter().filter(|call| *call == "query").count(),
        1,
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|call| call.starts_with("execute ") && call.contains(KNOWN))
    );
    assert!(
        fixture
            .sandbox
            .read(".loom/logs/gate/verifier-results.jsonl")
            .contains("mkDevShell invokes prek install")
    );
    assert!(calls.contains(&"opaque".into()));
}
#[test]
#[ignore = "requires actual verifier/CI/Loom and native Nix input providers"]
fn changed_owner_spec_selects_known_assertion() {
    let fixture = Fixture::new();
    fixture.fixture(false, false);
    let (result, calls) = fixture.gate("specs/fixture.md", "verify", true);
    assert!(result.status.success(), "{result:?}");
    assert!(
        calls
            .iter()
            .any(|call| call.starts_with("execute ") && call.contains(KNOWN))
    );
    assert_eq!(
        calls.iter().filter(|call| *call == "query").count(),
        1,
        "{calls:?}"
    );
}
#[test]
#[ignore = "requires actual verifier/CI/Loom and native Nix input providers"]
fn new_nix_definition_invalidates_known_assertion() {
    let fixture = Fixture::new();
    fixture.fixture(false, false);
    fixture.sandbox.write("new/definition.nix", "{}\n");
    let (result, calls) = fixture.gate("new/definition.nix", "verify", true);
    assert!(result.status.success(), "{result:?}");
    assert!(
        calls
            .iter()
            .any(|call| call.starts_with("execute ") && call.contains(KNOWN))
    );
}
#[test]
#[ignore = "requires actual verifier/CI/Loom and native Nix input providers"]
fn malformed_provider_document_is_a_blocking_protocol_finding() {
    let fixture = Fixture::new();
    fixture.fixture(true, false);
    let (result, calls) = fixture.gate("README.md", "verify", true);
    assert_eq!(result.status.code(), Some(1), "{result:?}");
    for message in ["input-query errored", "emitted a malformed inputs document"] {
        assert!(combined(&result).contains(message), "{result:?}");
    }
    assert_eq!(
        calls.iter().filter(|call| *call == "query").count(),
        1,
        "{calls:?}"
    );
}
#[test]
#[ignore = "requires actual verifier/CI/Loom and native Nix input providers"]
fn system_checks_remain_always_run_and_share_only_within_invocation() {
    let fixture = Fixture::new();
    fixture.fixture(false, true);
    for index in 0..2 {
        let (result, calls) = fixture.gate("README.md", "system", index == 0);
        assert!(result.status.success(), "{result:?}");
        assert_eq!(
            calls
                .iter()
                .filter(|call| *call == &format!("execute {KNOWN}"))
                .count(),
            1,
            "{calls:?}"
        );
        assert_eq!(
            calls.iter().filter(|call| *call == "query").count(),
            0,
            "{calls:?}"
        );
        let records = records(
            fixture
                .sandbox
                .read(".loom/logs/gate/verifier-results.jsonl")
                .as_bytes(),
        );
        assert!(records.len() >= 2);
        assert_eq!(
            records[records.len() - 2..]
                .iter()
                .map(|record| record["target"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [format!("verify:{KNOWN}"), format!("verify:{KNOWN}")]
        );
        assert!(!fixture.sandbox.root.join(".loom/marker.json").exists());
    }
}
