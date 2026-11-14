use super::process;
use serde_json::Value;
use std::fmt::Write as _;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::OnceLock,
};

pub struct Inputs {
    pub source: PathBuf,
    pub loom: PathBuf,
    pub system: String,
    store: String,
    nixpkgs: String,
}
fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}
fn success(output: Output) -> Output {
    assert!(
        output.status.success(),
        "{}{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    output
}
pub fn inputs() -> &'static Inputs {
    static INPUTS: OnceLock<Inputs> = OnceLock::new();
    INPUTS.get_or_init(|| {
        let source = PathBuf::from(
            std::env::var_os("WRIX_TEST_BROWSER_SOURCE").expect("owning source is required"),
        );
        let loom = PathBuf::from(
            std::env::var_os("WRIX_TEST_BROWSER_LOOM")
                .expect("consumed Loom executable is required"),
        );
        let system = std::env::var("WRIX_TEST_BROWSER_SYSTEM").unwrap();
        assert!(matches!(system.as_str(), "aarch64-linux" | "x86_64-linux"));
        let metadata: Value = serde_json::from_slice(
            &success(process::bounded(
                Command::new("nix")
                    .args(["flake", "metadata", "--json"])
                    .arg(format!("git+file://{}", source.display()))
                    .current_dir(&source),
            ))
            .stdout,
        )
        .unwrap();
        let store = metadata["path"].as_str().unwrap().to_owned();
        let expression = format!(
            "(builtins.getFlake {}).inputs.nixpkgs.outPath",
            serde_json::to_string(&format!("path:{store}")).unwrap()
        );
        let nixpkgs = text(
            &success(process::bounded(Command::new("nix").args([
                "eval",
                "--raw",
                "--impure",
                "--expr",
                &expression,
            ])))
            .stdout,
        );
        Inputs {
            source,
            loom,
            system,
            store,
            nixpkgs,
        }
    })
}
pub struct Fixture {
    _temp: tempfile::TempDir,
    pub root: PathBuf,
    env: BTreeMap<OsString, OsString>,
    sentinel: String,
}
impl Fixture {
    pub fn new() -> Self {
        let input = inputs();
        let temp = tempfile::Builder::new()
            .prefix("wrix-browser-scope-")
            .tempdir()
            .unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let runtime = root.join("runtime");
        fs::create_dir(&runtime).unwrap();
        let mut env: BTreeMap<_, _> = std::env::vars_os().collect();
        let locals = success(process::bounded(
            Command::new("git").args(["rev-parse", "--local-env-vars"]),
        ));
        for name in text(&locals.stdout).lines() {
            env.remove(OsStr::new(name));
        }
        env.retain(|name, _| {
            !name.as_encoded_bytes().starts_with(b"PLAYWRIGHT_") && name != "WRIX_MCP_MANIFEST"
        });
        for (name, value) in [
            ("LOOM_INSIDE", "1".into()),
            ("REPO_ROOT", root.as_os_str().to_owned()),
            ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
            ("GIT_CONFIG_NOSYSTEM", "1".into()),
            ("TMPDIR", runtime.into_os_string()),
        ] {
            env.insert(name.into(), value);
        }
        let sentinel = format!(
            "browser-runtime-{}",
            root.file_name().unwrap().to_str().unwrap()
        );
        let fixture = Self {
            _temp: temp,
            root,
            env,
            sentinel,
        };
        let pkgs = include_str!("../../fixtures/browser-scope/nixpkgs.nix.in")
            .replace(
                "@UPSTREAM@",
                &serde_json::to_string(&format!("path:{}", input.nixpkgs)).unwrap(),
            )
            .replace("@SYSTEM@", &input.system)
            .replace(
                "@SENTINEL@",
                &serde_json::to_string(&fixture.sentinel).unwrap(),
            );
        fixture.write("browser-nixpkgs/flake.nix", &pkgs);
        fixture.write(
            "flake.nix",
            &include_str!("../../fixtures/browser-scope/flake.nix.in")
                .replace(
                    "@SOURCE@",
                    &serde_json::to_string(&format!("path:{}", input.store)).unwrap(),
                )
                .replace("@SYSTEM@", &serde_json::to_string(&input.system).unwrap()),
        );
        let mut config = fs::read_to_string(input.source.join("loom.toml")).unwrap();
        for (name, command) in [
            ("verify", "nix run .#verify --"),
            ("test-ci", "bin/test-ci-verifiers"),
        ] {
            config = config.replace(
                &format!("inputs = \"{command} {{print_inputs}} {{targets}}\""),
                &format!("inputs = \"bash query-provider.sh {name} {{print_inputs}} {{targets}}\""),
            );
        }
        fixture.write("loom.toml", &config);
        fixture.write("query-provider.sh", "set -euo pipefail\nprintf '%s\\n' \"$*\" >> \"$REPO_ROOT/query-calls\"\nkind=\"$1\"\nshift\nif [[ \"$kind\" == verify ]]; then\n  exec nix run .#verify -- \"$@\"\nfi\nexec bin/test-ci-verifiers \"$@\"\n");
        for directory in ["bin", "tests/mcp/playwright", "lib/mcp"] {
            copy_tree(&input.source.join(directory), &fixture.root.join(directory));
        }
        fixture.write(".pre-commit-config.yaml", &serde_json::json!({"repos":[{"repo":"local","hooks":[{"id":"fixture","name":"fixture","entry":"true","language":"system","always_run":true,"pass_filenames":false,"stages":["pre-commit"]}]}]}).to_string());
        let mut spec = String::from("# Fixture\n\n## Success Criteria\n\n");
        for tier in ["check", "system"] {
            for target in [
                "verify:playwright-mcp.screenshot",
                "test-ci:test-playwright-chromium-executable-path",
            ] {
                writeln!(spec, "- Browser boundary [{tier}]({target})").unwrap();
            }
        }
        for tier in ["check", "system"] {
            writeln!(
                spec,
                "- Opaque information [{tier}](verify:beads.tracked-files)"
            )
            .unwrap();
        }
        fixture.write("specs/fixture.md", &spec);
        fixture.write("docs/README.md", "# Overview\n\n| Spec | Code | Beads | Purpose |\n| --- | --- | --- | --- |\n| [fixture](../specs/fixture.md) | tests/ | — | Scope fixture |\n| [playwright-mcp](../specs/playwright-mcp.md) | lib/mcp/ | — | Browser |\n| [cache](../specs/cache.md) | crates/wrix-cache/ | — | Cache |\n");
        for path in [
            "specs/playwright-mcp.md",
            "tests/mcp/playwright/screenshot-test.sh",
            "tests/sandbox/pi-mcp-native.mjs",
            "lib/sandbox/image.nix",
            "tests/verify/default.nix",
            ".beads/.gitignore",
            ".beads/config.yaml",
            ".beads/metadata.json",
        ] {
            fixture.write(path, &fs::read_to_string(input.source.join(path)).unwrap());
        }
        fixture.write(
            "specs/playwright-mcp.md",
            "# Browser owning contract fixture\n",
        );
        fixture.write("specs/cache.md", "# Cache owning contract fixture\n");
        fixture.write(
            "crates/wrix-cache/src/recovery.rs",
            "// Cache recovery source fixture\n",
        );
        fixture.ok("git", &["init", "-q"]);
        fixture.ok("git", &["add", "-f", "."]);
        fixture.ok("nix", &["flake", "lock"]);
        fixture.ok("git", &["add", "-f", "."]);
        fixture.commit();
        fixture
    }
    pub fn write(&self, relative: &str, data: &str) {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, data).unwrap();
    }
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(program);
        command.env_clear().envs(&self.env).current_dir(&self.root);
        command
    }
    pub fn run(&self, program: impl AsRef<OsStr>, args: &[&str]) -> Output {
        process::bounded(self.command(program).args(args))
    }
    pub fn ok(&self, program: impl AsRef<OsStr>, args: &[&str]) -> Output {
        success(self.run(program, args))
    }
    pub fn commit(&self) {
        self.ok("git", &["add", "-u"]);
        self.ok(
            "git",
            &[
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
                "Record scoped subject",
            ],
        );
    }
    pub fn scoped_gate(&self, lane: &str, path: &str) -> (Output, Vec<Value>, Vec<String>) {
        let file = self.root.join(path);
        let mut data = fs::read_to_string(&file).unwrap();
        data.push('\n');
        fs::write(file, data).unwrap();
        self.commit();
        let receipts = self.root.join(".loom/logs/gate/verifier-results.jsonl");
        if receipts.exists() {
            fs::remove_file(&receipts).unwrap();
        }
        self.write("query-calls", "");
        let result = self.run(&inputs().loom, &["gate", lane, "--diff", "HEAD^..HEAD"]);
        let queries = fs::read_to_string(self.root.join("query-calls"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        let records = if receipts.exists() {
            fs::read_to_string(receipts)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        } else {
            Vec::new()
        };
        (result, records, queries)
    }
    pub fn gate(&self, lane: &str, path: &str) -> (Output, Vec<Value>) {
        let (result, records, queries) = self.scoped_gate(lane, path);
        let groups = if lane == "verify" { 2 } else { 1 };
        assert_eq!(
            queries.len(),
            2 * groups,
            "{} {queries:?}",
            combined(&result)
        );
        for kind in ["verify", "test-ci"] {
            assert_eq!(
                queries
                    .iter()
                    .filter(|line| line.starts_with(&format!("{kind} ")))
                    .count(),
                groups,
                "{queries:?}"
            );
        }
        (result, records)
    }
    pub fn boundaries(&self) -> Vec<String> {
        serde_json::from_slice(
            &self
                .ok(
                    "nix",
                    &[
                        "eval",
                        "--json",
                        &format!(".#legacyPackages.{}.browserBoundaries", inputs().system),
                    ],
                )
                .stdout,
        )
        .unwrap()
    }
    pub fn logs(&self) -> String {
        self.boundaries()
            .into_iter()
            .map(|path| {
                let result = self.run("nix", &["log", &path]);
                if !result.status.success() {
                    assert!(text(&result.stderr).contains("is not available"));
                }
                text(&result.stdout)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    pub fn cold(&self) {
        let logs = self.logs();
        assert!(!logs.contains("BROWSER_RUNTIME_REALIZED"));
        assert!(!logs.contains("IMAGE_RUNTIME_REALIZED"));
    }
    pub fn realized(&self, count: usize) {
        assert!(self.logs().matches("BROWSER_RUNTIME_REALIZED").count() >= count);
    }
    pub fn absent_runtime(&self, result: &Output) {
        assert!(!combined(result).contains(&self.sentinel));
    }
    pub fn no_approval(&self) {
        assert!(!self.root.join(".loom/marker.json").exists());
    }
}
pub fn combined(result: &Output) -> String {
    format!("{}{}", text(&result.stdout), text(&result.stderr))
}
fn copy_tree(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let destination = target.join(entry.file_name());
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            copy_tree(&path, &destination);
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(path).unwrap(), destination).unwrap();
        } else {
            fs::copy(path, destination).unwrap();
        }
    }
}
