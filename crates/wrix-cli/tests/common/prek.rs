use std::{env, fs, path::PathBuf, process::Command, sync::OnceLock};

use super::{TestResult, run_command};

pub struct Runtime {
    pub hooks: PathBuf,
    pub runner: PathBuf,
    pub broken_runner: PathBuf,
    pub builder_spec: PathBuf,
}

pub fn runtime() -> TestResult<&'static Runtime> {
    static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| build_runtime().map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|error| error.clone().into())
}

pub fn packaged_wrix() -> TestResult<PathBuf> {
    if let Some(path) = env::var_os("WRIX_TEST_PACKAGED_WRIX") {
        return Ok(PathBuf::from(path));
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let result = run_command(Command::new("nix").current_dir(root).args([
        "build",
        "--no-link",
        "--print-out-paths",
        ".#wrix",
    ]))?;
    if !result.status.success() {
        return Err(format!("cannot build packaged Wrix launcher: {}", result.stderr).into());
    }
    Ok(PathBuf::from(result.stdout.trim()).join("bin/wrix"))
}

pub fn devshell_hook() -> TestResult<String> {
    if let Some(path) = env::var_os("WRIX_TEST_DEVSHELL_HOOK") {
        return Ok(fs::read_to_string(path)?);
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let result = run_command(
        Command::new("nix")
            .args([
                "eval",
                "--raw",
                "--impure",
                "--expr",
                r#"let
          flake = builtins.getFlake ("git+file://" + builtins.getEnv "WRIX_TEST_REPO");
          lib = flake.legacyPackages.${builtins.currentSystem}.lib;
        in (lib.mkDevShell { profile = lib.profiles.base; nixCache = false; }).shellHook"#,
            ])
            .env("WRIX_TEST_REPO", root),
    )?;
    if !result.status.success() {
        return Err(format!("cannot evaluate devshell hook: {}", result.stderr).into());
    }
    Ok(result.stdout)
}

fn build_runtime() -> TestResult<Runtime> {
    if let (Some(hooks), Some(runner), Some(broken_runner), Some(builder_spec)) = (
        env::var_os("WRIX_TEST_PREK_HOOKS"),
        env::var_os("WRIX_TEST_PREK_RUNNER"),
        env::var_os("WRIX_TEST_BROKEN_PREK_RUNNER"),
        env::var_os("WRIX_TEST_PREK_BUILD_SPEC"),
    ) {
        return Ok(Runtime {
            hooks: PathBuf::from(hooks),
            runner: PathBuf::from(runner),
            broken_runner: PathBuf::from(broken_runner),
            builder_spec: PathBuf::from(builder_spec),
        });
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let expression = r#"let
      root = builtins.toPath (builtins.getEnv "WRIX_TEST_REPO");
      flake = builtins.getFlake ("git+file://" + toString root);
      pkgs = import flake.inputs.nixpkgs { system = builtins.currentSystem; };
    in pkgs.linkFarm "wrix-test-prek-runtime" [
      { name = "hooks"; path = import (root + "/lib/prek/bundle.nix") { inherit pkgs; }; }
      { name = "runner"; path = import (root + "/lib/prek/runner.nix") { inherit pkgs; }; }
      { name = "broken-runner"; path = import (root + "/tests/prek/missing-runtime.nix") { inherit pkgs; }; }
      { name = "builder-spec"; path = import (root + "/tests/prek/bundle-build-spec.nix") { inherit pkgs; }; }
    ]"#;
    let result = run_command(
        Command::new("nix")
            .args([
                "build",
                "--impure",
                "--no-link",
                "--print-out-paths",
                "--expr",
                expression,
            ])
            .env("WRIX_TEST_REPO", fs::canonicalize(root)?),
    )?;
    if !result.status.success() {
        return Err(format!("cannot build real packaged hook runtime: {}", result.stderr).into());
    }
    let path = PathBuf::from(result.stdout.trim());
    Ok(Runtime {
        hooks: fs::canonicalize(path.join("hooks"))?,
        runner: fs::canonicalize(path.join("runner/bin/wrix-prek"))?,
        broken_runner: fs::canonicalize(path.join("broken-runner/bin/wrix-prek"))?,
        builder_spec: fs::canonicalize(path.join("builder-spec"))?,
    })
}
