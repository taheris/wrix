#![cfg(target_os = "linux")]
#[cfg(test)]
#[path = "browser_gate_scope/fixture/mod.rs"]
mod fixture;
#[cfg(test)]
#[path = "browser_gate_scope/process/mod.rs"]
mod process;
#[cfg(test)]
mod test {
    use super::fixture::{Fixture, combined, inputs};
    use serde_json::Value;
    use std::{fmt::Write, fs, sync::Mutex, thread};
    fn browser_target(record: &Value) -> bool {
        matches!(
            record["target"].as_str(),
            Some(
                "verify:playwright-mcp.screenshot"
                    | "test-ci:test-playwright-chromium-executable-path"
            )
        )
    }
    fn passed(result: &std::process::Output) {
        assert!(result.status.success(), "{}", combined(result));
    }
    #[test]
    #[ignore = "Executed by the Linux cli.browser-gate-scope owner with real Nix and consumed Loom."]
    fn boundary_fixture_fails_only_when_realized() {
        let f = Fixture::new();
        f.cold();
        for path in f.boundaries() {
            let result = f.run("nix", &["build", "--no-link", &format!("{path}^*")]);
            assert!(!result.status.success(), "{}", combined(&result));
        }
        f.realized(2);
    }
    #[test]
    #[ignore = "Executed by the Linux cli.browser-gate-scope owner with real Nix and consumed Loom."]
    fn inventory_and_description_do_not_realize_browser_boundary() {
        let f = Fixture::new();
        f.cold();
        for app in ["verify", "test-ci"] {
            for mode in ["--list", "--print-inputs"] {
                let result = f.ok("nix", &["run", &format!(".#{app}"), "--", mode]);
                f.absent_runtime(&result);
                assert!(!combined(&result).contains("BROWSER_RUNTIME_REALIZED"));
            }
        }
        f.cold();
    }
    #[test]
    #[ignore = "Executed by the Linux cli.browser-gate-scope owner with real Nix and consumed Loom."]
    fn unrelated_documentation_excludes_browser_in_both_lanes() {
        let f = Fixture::new();
        let mut known = Vec::new();
        for (app, prefix) in [("verify", "verify:"), ("test-ci", "test-ci:")] {
            let value: Value = serde_json::from_slice(
                &f.ok("nix", &["run", &format!(".#{app}"), "--", "--print-inputs"])
                    .stdout,
            )
            .unwrap();
            for target in value["inputs"].as_object().unwrap().keys() {
                known.push(format!("{prefix}{target}"));
            }
        }
        let mut spec = String::from("# Fixture\n\n## Success Criteria\n\n");
        for tier in ["check", "system"] {
            for target in known
                .iter()
                .map(String::as_str)
                .chain(["verify:beads.tracked-files"])
            {
                writeln!(spec, "- Declared resource [{tier}]({target})").unwrap();
            }
        }
        f.write("specs/fixture.md", &spec);
        f.commit();
        f.cold();
        for lane in ["verify", "system"] {
            let (result, records) = f.gate(lane, "docs/README.md");
            passed(&result);
            assert!(
                !records
                    .iter()
                    .any(|record| known.iter().any(|target| record["target"] == *target))
            );
            f.absent_runtime(&result);
            assert!(
                records
                    .iter()
                    .any(|record| record["target"] == "verify:beads.tracked-files")
            );
        }
        f.cold();
    }
    #[test]
    #[ignore = "Executed by the Linux cli.browser-gate-scope owner with real Nix and consumed Loom."]
    fn cache_changes_leave_browser_and_profile_image_boundaries_cold() {
        let f = Fixture::new();
        let mut spec = fs::read_to_string(f.root.join("specs/fixture.md")).unwrap();
        for tier in ["check", "system"] {
            writeln!(
                spec,
                "- Image graph [{tier}](test-ci:test-image-tier-graph)"
            )
            .unwrap();
        }
        f.write("specs/fixture.md", &spec);
        f.commit();
        for lane in ["verify", "system"] {
            for path in ["crates/wrix-cache/src/recovery.rs", "specs/cache.md"] {
                let (result, records, queries) = f.scoped_gate(lane, path);
                passed(&result);
                assert!(!queries.is_empty());
                assert!(!records.is_empty());
                assert!(!records.iter().any(browser_target));
                assert!(
                    !records
                        .iter()
                        .any(|record| record["target"] == "test-ci:test-image-tier-graph")
                );
                f.cold();
            }
        }
    }
    #[test]
    #[ignore = "Executed by the Linux cli.browser-gate-scope owner with real Nix and consumed Loom."]
    fn relevant_changes_reach_cold_browser_runtime_in_both_lanes() {
        let paths = [
            "tests/mcp/playwright/screenshot-test.sh",
            "tests/sandbox/pi-mcp-native.mjs",
            "specs/playwright-mcp.md",
            "flake.lock",
            "lib/sandbox/image.nix",
            "tests/verify/default.nix",
        ];
        let cases = Mutex::new(
            ["verify", "system"]
                .into_iter()
                .flat_map(|lane| paths.map(|path| (lane, path)))
                .collect::<Vec<_>>(),
        );
        thread::scope(|scope| {
            let mut workers = Vec::new();
            for _ in 0..3 {
                workers.push(scope.spawn(|| {
                    loop {
                        let case = cases.lock().unwrap().pop();
                        let Some((lane, path)) = case else {
                            break;
                        };
                        let f = Fixture::new();
                        f.cold();
                        let (result, records) = f.gate(lane, path);
                        assert!(
                            !result.status.success(),
                            "{lane} {path}: {} records={records:?}",
                            combined(&result)
                        );
                        for target in [
                            "verify:playwright-mcp.screenshot",
                            "test-ci:test-playwright-chromium-executable-path",
                        ] {
                            assert!(
                                records.iter().any(|record| record["target"] == target),
                                "{}",
                                combined(&result)
                            );
                        }
                        f.realized(2);
                        for record in records.iter().filter(|record| browser_target(record)) {
                            assert_eq!(record["outcome"], "fail");
                        }
                        f.no_approval();
                    }
                }));
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
    }
    #[test]
    #[ignore = "Executed by the Linux cli.browser-gate-scope owner with real Nix and consumed Loom."]
    fn explicit_and_full_verification_keep_browser_coverage() {
        for lane in ["verify", "system"] {
            for full in [true, false] {
                let f = Fixture::new();
                f.cold();
                let command_lane = if lane == "verify" && !full {
                    "check"
                } else {
                    lane
                };
                let scope = if full {
                    vec!["--tree"]
                } else {
                    vec![
                        "--target",
                        "test-ci:test-playwright-chromium-executable-path",
                    ]
                };
                let mut args = vec!["gate", command_lane];
                args.extend(scope);
                let result = f.run(&inputs().loom, &args);
                assert!(!result.status.success(), "{}", combined(&result));
                f.realized(if full { 2 } else { 1 });
                f.no_approval();
            }
        }
    }
    #[test]
    #[ignore = "Executed by the Linux cli.browser-gate-scope owner with real Nix and consumed Loom."]
    fn bad_discovery_blocks_both_lanes_before_realization() {
        let f = Fixture::new();
        for response in [
            "printf '%s\\n' '{\"inputs\":{\"playwright-mcp.screenshot\":\"malformed\"}}'",
            "exit 9",
        ] {
            f.write(
                "query-provider.sh",
                &format!("set -euo pipefail\n{response}\n"),
            );
            f.commit();
            for lane in ["verify", "system"] {
                let result = f.run(&inputs().loom, &["gate", lane, "--diff", "HEAD^..HEAD"]);
                assert!(!result.status.success());
                assert!(
                    combined(&result).contains("input-query errored"),
                    "{}",
                    combined(&result)
                );
                assert!(!combined(&result).contains("BROWSER_RUNTIME_REALIZED"));
            }
        }
    }
}
