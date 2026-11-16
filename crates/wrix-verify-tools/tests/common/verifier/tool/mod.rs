use super::fixture::{Sandbox, capture, combined};
use serde::Deserialize;
use std::{
    env,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
pub struct Set {
    pub source: PathBuf,
    pub bash: PathBuf,
    pub coreutils: PathBuf,
    pub jq: PathBuf,
}

impl Set {
    pub fn load(sandbox: &Sandbox) -> Self {
        let source = env::var_os("WRIX_TEST_SOURCE").unwrap();
        let reference =
            serde_json::to_string(&format!("git+file://{}", Path::new(&source).display())).unwrap();
        let system = serde_json::to_string(&env::var("WRIX_TEST_SYSTEM").unwrap()).unwrap();
        let expression = format!(
            "let f = builtins.getFlake {reference}; p = f.inputs.nixpkgs.legacyPackages.{system}; in {{ source = f.inputs.loom-src.outPath; bash = p.bash.outPath; coreutils = p.coreutils.outPath; jq = p.jq.outPath; }}"
        );
        let result = capture(sandbox.command("nix").args([
            "eval",
            "--impure",
            "--json",
            "--expr",
            &expression,
        ]));
        assert!(result.status.success(), "{}", combined(&result));
        serde_json::from_slice(&result.stdout).unwrap()
    }
}
