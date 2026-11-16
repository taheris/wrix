use super::{Fixture, binary, combined};
use serde_json::Value;

#[derive(Clone, Copy)]
enum Definition {
    Check,
    OwnedCheck,
    Report,
    SystemReport,
    WorkspaceReport { system: bool },
}

impl Definition {
    fn is_system(self) -> bool {
        matches!(
            self,
            Self::SystemReport | Self::WorkspaceReport { system: true }
        )
    }

    fn is_report(self) -> bool {
        matches!(
            self,
            Self::Report | Self::SystemReport | Self::WorkspaceReport { .. }
        )
    }
}

fn fixture(definition: Definition) -> Fixture {
    let fixture = Fixture::new();
    let system = serde_json::to_string(&fixture.system).unwrap();
    let inputs = super::checked_tool::Set::load(&fixture.sandbox);
    let template = match definition {
        Definition::Check => include_str!("../../fixtures/checked-verifier/flake.nix.in"),
        Definition::OwnedCheck => include_str!("../../fixtures/checked-verifier/owned.nix.in"),
        Definition::Report | Definition::SystemReport => {
            include_str!("../../fixtures/checked-verifier/report.nix.in")
        }
        Definition::WorkspaceReport { .. } => {
            include_str!("../../fixtures/checked-verifier/workspace-report.nix.in")
        }
    };
    let flake = template
        .replace(
            "@loomSource@",
            &serde_json::to_string(&format!("path:{}", inputs.source.display())).unwrap(),
        )
        .replace("@bash@", &serde_json::to_string(&inputs.bash).unwrap())
        .replace(
            "@coreutils@",
            &serde_json::to_string(&inputs.coreutils).unwrap(),
        )
        .replace("@jq@", &serde_json::to_string(&inputs.jq).unwrap())
        .replace("@system@", &system)
        .replace(
            "@nixpkgsSource@",
            &serde_json::to_string(&format!("path:{}", binary("WRIX_TEST_NIXPKGS").display()))
                .unwrap(),
        )
        .replace(
            "@tier@",
            if definition.is_system() {
                "\"system\""
            } else {
                "\"check\""
            },
        );
    fixture.sandbox.write("flake.nix", &flake);
    fixture.sandbox.write(
        ".gitignore",
        ".loom/\nhome/\nconfig/\nstate/\ncache/\ncalls\n",
    );
    fixture.sandbox.write("README.md", "# Native fixture\n");
    fixture.sandbox.write("loom.toml", "");
    fixture.sandbox.write(
        "docs/README.md",
        "# Fixture\n\n- [fixture](../specs/fixture.md)\n",
    );
    let target = match definition {
        Definition::Check => format!("nix build .#checks.{}.heading", fixture.system),
        Definition::OwnedCheck => format!("nix build .#checks.{}.devshell", fixture.system),
        Definition::Report | Definition::SystemReport | Definition::WorkspaceReport { .. } => {
            format!("nix report .#checks.{}.report::heading", fixture.system)
        }
    };
    let tier = if definition.is_system() {
        "system"
    } else {
        "check"
    };
    let criterion = if matches!(definition, Definition::OwnedCheck) {
        "The actual Wrix devshell.no-prek-install assertion passes."
    } else {
        "The captured subject has a heading."
    };
    fixture.sandbox.write(
        "specs/fixture.md",
        &format!("# Native fixture\n\n## Success Criteria\n\n- {criterion} [{tier}]({target})\n"),
    );
    if matches!(definition, Definition::WorkspaceReport { .. }) {
        let contract = fixture.sandbox.read("specs/fixture.md");
        fixture.sandbox.write("specs/fixture.md", &format!("{contract}- A scoped sibling retains independent coverage. [check](nix build .#checks.{}.specific)\n", fixture.system));
    }
    if definition.is_report() {
        fixture.sandbox.write(
            "reports/verifier.sh",
            &std::fs::read_to_string(binary("WRIX_TEST_SOURCE").join("tests/lib/verifier.sh"))
                .unwrap(),
        );
        fixture.sandbox.write(
            "reports/requirements.json",
            "{\"platforms\":[],\"capabilities\":[]}\n",
        );
        fixture.sandbox.write("subjects/command.sh", "#!/usr/bin/env bash\nset -euo pipefail\nread -r heading < \"$1\"\n[[ \"$heading\" == '# '* ]]\n");
    }
    fixture
        .sandbox
        .write("subjects/README.md", "# Captured subject\n");
    if matches!(definition, Definition::OwnedCheck) {
        for relative in [
            "tests/verify/profiles-eval.nix",
            "tests/verify/agent-defaults.nix",
            "tests/verify/network-rules.nix",
            "tests/lib/inputs.nix",
            "tests/lib/browser-resources.nix",
        ] {
            fixture.sandbox.write(
                &format!("subjects/{relative}"),
                &std::fs::read_to_string(binary("WRIX_TEST_SOURCE").join(relative)).unwrap(),
            );
        }
        fixture.sandbox.write(
            "subjects/lib/devshell/default.nix",
            "# Consumer construction does not mutate hooks.\n",
        );
    }
    for args in [
        vec!["init", "-q"],
        vec!["-c", "core.hooksPath=/dev/null", "add", "."],
    ] {
        let output = fixture.command("git", &args);
        assert!(output.status.success(), "{}", combined(&output));
    }
    let lock = fixture.command("nix", &["flake", "lock"]);
    assert!(lock.status.success(), "{}", combined(&lock));
    let tracked = fixture.command(
        "git",
        &["-c", "core.hooksPath=/dev/null", "add", "flake.lock"],
    );
    assert!(tracked.status.success(), "{}", combined(&tracked));
    let committed = fixture.command(
        "git",
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "Define actual checked fixture",
        ],
    );
    assert!(committed.status.success(), "{}", combined(&committed));
    fixture
}

pub fn owned_source() -> Fixture {
    fixture(Definition::OwnedCheck)
}

pub fn workspace_report(system: bool) -> Fixture {
    fixture(Definition::WorkspaceReport { system })
}

#[derive(Clone, Copy)]
pub enum Selection {
    Tree,
    File(&'static str),
}

fn gate(fixture: &Fixture) -> std::process::Output {
    scoped_gate(fixture, Selection::Tree)
}

pub fn scoped_gate(fixture: &Fixture, selection: Selection) -> std::process::Output {
    let root = fixture.sandbox.root.to_str().unwrap();
    let mut arguments = vec!["gate", "verify", "--workspace", root];
    match selection {
        Selection::Tree => arguments.push("--tree"),
        Selection::File(path) => arguments.extend(["--files", path]),
    }
    let output = fixture.command(binary("WRIX_TEST_LOOM").to_str().unwrap(), &arguments);
    assert!(
        !combined(&output).contains("no actual checked definition"),
        "{}",
        combined(&output)
    );
    output
}

#[test]
#[ignore = "requires the actual consumed Loom CLI and native Nix backend"]
fn checked_owned_wrix_assertion_preserves_real_failure_and_restoration() {
    let fixture = fixture(Definition::OwnedCheck);
    let first = gate(&fixture);
    assert!(first.status.success(), "{}", combined(&first));
    fixture
        .sandbox
        .write("subjects/lib/devshell/default.nix", "\"prek install\"\n");
    let changed = gate(&fixture);
    assert!(!changed.status.success(), "{}", combined(&changed));
    assert!(
        combined(&changed).contains("mkDevShell invokes prek install"),
        "{}",
        combined(&changed)
    );
    fixture.sandbox.write(
        "subjects/lib/devshell/default.nix",
        "# Consumer construction does not mutate hooks.\n",
    );
    let restored = gate(&fixture);
    assert!(restored.status.success(), "{}", combined(&restored));
}

#[test]
#[ignore = "requires the actual consumed Loom CLI and native Nix backend"]
fn checked_source_change_executes_the_native_assertion_instead_of_reusing_a_pass() {
    let fixture = fixture(Definition::Check);
    let first = gate(&fixture);
    assert!(first.status.success(), "{}", combined(&first));
    assert!(
        combined(&first).contains("execution-graph"),
        "{}",
        combined(&first)
    );
    fixture
        .sandbox
        .write("subjects/README.md", "Missing heading\n");
    let changed = gate(&fixture);
    assert!(!changed.status.success(), "{}", combined(&changed));
    assert!(
        combined(&changed).contains("builder failed with exit code 1"),
        "{}",
        combined(&changed)
    );
    fixture
        .sandbox
        .write("subjects/README.md", "# Captured subject\n");
    let restored = gate(&fixture);
    assert!(restored.status.success(), "{}", combined(&restored));
    assert_eq!(fixture.sandbox.read("calls"), "");
}

#[test]
#[ignore = "requires the actual consumed Loom CLI and native Nix backend"]
fn a_new_unregistered_nix_definition_cannot_borrow_an_existing_checks_coverage() {
    let fixture = fixture(Definition::Check);
    let original = gate(&fixture);
    assert!(original.status.success(), "{}", combined(&original));
    fixture.sandbox.write("unregistered.nix", "{}\n");
    let result = scoped_gate(&fixture, Selection::File("README.md"));
    assert!(!result.status.success(), "{}", combined(&result));
    assert!(
        combined(&result).contains("Evaluate coverage"),
        "{}",
        combined(&result)
    );
    assert!(
        combined(&result).contains("missing required Evaluate coverage"),
        "{}",
        combined(&result)
    );
}

#[test]
#[ignore = "requires the actual consumed Loom CLI and native Nix backend"]
fn a_native_report_consumes_the_actual_wrix_verifier_result_and_retains_failure() {
    let fixture = fixture(Definition::Report);
    let first = gate(&fixture);
    assert!(first.status.success(), "{}", combined(&first));
    assert!(
        combined(&first).contains("fresh-execution"),
        "{}",
        combined(&first)
    );
    assert!(
        combined(&first).contains("::heading"),
        "{}",
        combined(&first)
    );
    fixture
        .sandbox
        .write("subjects/README.md", "Missing heading\n");
    let changed = gate(&fixture);
    assert_eq!(changed.status.code(), Some(1), "{}", combined(&changed));
    assert!(
        combined(&changed).contains("\"verdict\":\"fail\""),
        "{}",
        combined(&changed)
    );
    assert!(
        combined(&changed).contains("::heading"),
        "{}",
        combined(&changed)
    );
}

#[test]
#[ignore = "requires the actual consumed Loom CLI and native Nix backend"]
fn producer_foreign_skip_metadata_cannot_authorize_worker_coverage() {
    let fixture = fixture(Definition::Report);
    let foreign = if fixture.system.ends_with("-darwin") {
        "x86_64-linux"
    } else {
        "aarch64-darwin"
    };
    fixture.sandbox.write(
        "reports/requirements.json",
        &serde_json::json!({"platforms":[foreign],"capabilities":[]}).to_string(),
    );
    let result = gate(&fixture);
    assert_eq!(result.status.code(), Some(78), "{}", combined(&result));
    assert!(
        combined(&result).contains("coverage unverified, permitted=false"),
        "{}",
        combined(&result)
    );
    assert!(
        combined(&result).contains("\"kind\":\"foreign-platform\""),
        "{}",
        combined(&result)
    );
    assert!(
        combined(&result).contains("fresh-execution"),
        "{}",
        combined(&result)
    );
}

#[test]
#[ignore = "requires the actual consumed Loom CLI and native Nix backend"]
fn a_native_wrix_report_preserves_unreported_exit_77_as_unverified() {
    let fixture = fixture(Definition::Report);
    fixture.sandbox.write("subjects/command.sh", "#!/usr/bin/env bash\nset -euo pipefail\nprintf 'unreported fixture prerequisite\\n' >&2\nexit 77\n");
    let result = gate(&fixture);
    assert_eq!(result.status.code(), Some(1), "{}", combined(&result));
    assert!(
        combined(&result).contains("unexpected skip:"),
        "{}",
        combined(&result)
    );
    assert!(
        combined(&result).contains("permitted=false"),
        "{}",
        combined(&result)
    );
    assert!(
        !combined(&result).contains("\"kind\":\"foreign-platform\""),
        "{}",
        combined(&result)
    );
}

#[test]
#[ignore = "requires the actual consumed Loom CLI and native Nix backend"]
fn a_native_wrix_report_cannot_hide_an_intermediate_assertion_failure() {
    let fixture = fixture(Definition::Report);
    fixture.sandbox.write(
        "subjects/command.sh",
        "#!/usr/bin/env bash\nset -euo pipefail\nfalse\nprintf 'concealed success\\n'\n",
    );
    let result = gate(&fixture);
    assert_eq!(result.status.code(), Some(1), "{}", combined(&result));
    assert!(
        combined(&result).contains("\"verdict\":\"fail\""),
        "{}",
        combined(&result)
    );
    assert!(
        !combined(&result).contains("concealed success"),
        "{}",
        combined(&result)
    );
}

pub fn plan(output: &std::process::Output) -> Value {
    combined(output)
        .lines()
        .find_map(|line| {
            line.strip_prefix("loom gate plan: ")
                .map(|json| serde_json::from_str(json).unwrap())
        })
        .unwrap()
}

#[test]
#[ignore = "requires the actual consumed Loom CLI and native Nix backend"]
fn an_independent_document_change_does_not_execute_an_unrelated_native_assertion() {
    let fixture = fixture(Definition::Check);
    fixture
        .sandbox
        .write("README.md", "# Changed independent documentation\n");
    let result = scoped_gate(&fixture, Selection::File("README.md"));
    assert!(result.status.success(), "{}", combined(&result));
    let selected = plan(&result);
    assert!(
        selected["executions"].as_array().unwrap().iter().all(
            |entry| entry["target"] != format!("nix build .#checks.{}.heading", fixture.system)
        ),
        "{selected}"
    );
}

#[test]
#[ignore = "requires the actual consumed Loom CLI and native Nix backend"]
fn a_native_owner_contract_change_selects_its_actual_checked_assertion() {
    let fixture = fixture(Definition::Check);
    let contract = fixture.sandbox.read("specs/fixture.md");
    fixture.sandbox.write(
        "specs/fixture.md",
        &format!("{contract}\nOwning contract clarification.\n"),
    );
    let result = scoped_gate(&fixture, Selection::File("specs/fixture.md"));
    assert!(result.status.success(), "{}", combined(&result));
    let selected = plan(&result);
    assert!(
        selected["executions"].as_array().unwrap().iter().any(
            |entry| entry["target"] == format!("nix build .#checks.{}.heading", fixture.system)
        ),
        "{selected}"
    );
    assert!(
        combined(&result).contains("execution-graph"),
        "{}",
        combined(&result)
    );
}

pub fn fresh_invocations(output: &std::process::Output) -> std::collections::BTreeSet<String> {
    let mut invocations = std::collections::BTreeSet::new();
    for line in combined(output).lines() {
        if !line.starts_with("loom gate [") {
            continue;
        }
        let Some((_, json)) = line.split_once("]: ") else {
            continue;
        };
        if !json.starts_with('{') {
            continue;
        }
        let records = super::records(json.as_bytes());
        assert_eq!(records.len(), 1);
        for observation in records[0]["observations"].as_array().unwrap() {
            if observation["kind"] == "fresh-execution" {
                invocations.insert(observation["derivation"].as_str().unwrap().to_owned());
            }
        }
    }
    invocations
}

#[test]
#[ignore = "requires the actual consumed Loom CLI and native Nix backend"]
fn system_report_owners_share_only_the_current_fresh_invocation() {
    let fixture = fixture(Definition::SystemReport);
    let contract = fixture.sandbox.read("specs/fixture.md");
    fixture.sandbox.write("specs/fixture.md", &format!("{contract}\n- A second owner retains its own assertion. [system](nix report .#checks.{}.report::heading)\n", fixture.system));
    let mut previous = std::collections::BTreeSet::new();
    for _ in 0..2 {
        let result = scoped_gate(&fixture, Selection::File("subjects/README.md"));
        assert!(result.status.success(), "{}", combined(&result));
        let invocations = fresh_invocations(&result);
        assert_eq!(invocations.len(), 1, "{}", combined(&result));
        assert!(
            invocations.is_disjoint(&previous),
            "{previous:?} {invocations:?}"
        );
        previous = invocations;
    }
}

#[test]
#[ignore = "requires the actual consumed Loom CLI and native Nix input providers"]
fn malformed_legacy_input_json_cannot_authorize_an_unregistered_execution() {
    let fixture = Fixture::new();
    fixture.fixture(true, false);
    let (result, calls) = fixture.gate("README.md", "verify", true);
    assert_eq!(result.status.code(), Some(1), "{}", combined(&result));
    assert!(
        combined(&result).contains("no actual checked definition"),
        "{}",
        combined(&result)
    );
    assert_eq!(calls, Vec::<String>::new());
}
