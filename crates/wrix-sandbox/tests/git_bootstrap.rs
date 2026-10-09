use std::{fs, path::Path, process::Command};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn bootstrap(root: &Path, script: &str) -> TestResult {
    let home = root.join("home");
    fs::create_dir_all(&home)?;
    let helper = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lib/util/git-ssh-setup.sh");
    let output = Command::new("bash")
        .args(["-c", script, "bootstrap-test"])
        .arg(helper)
        .current_dir(root)
        .env("HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_CONFIG_GLOBAL")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_SSH_COMMAND")
        .env_remove("SSH_AUTH_SOCK")
        .env_remove("WRIX_DEPLOY_KEY")
        .env_remove("WRIX_SIGNING_KEY")
        .env("GIT_AUTHOR_NAME", "Wrix Test")
        .env("GIT_AUTHOR_EMAIL", "wrix@example.test")
        .env("GIT_COMMITTER_NAME", "Wrix Test")
        .env("GIT_COMMITTER_EMAIL", "wrix@example.test")
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "wrix.fixture")
        .env("GIT_CONFIG_VALUE_0", "retained")
        .env("GIT_CONFIG_KEY_1", "commit.gpgsign")
        .env("GIT_CONFIG_VALUE_1", "true")
        .output()?;
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
fn sign_disabled_overrides_shared_and_worktree_policy_without_keys() -> TestResult {
    let root = tempfile::tempdir()?;
    bootstrap(
        root.path(),
        r#"
set -euo pipefail
unset GIT_CONFIG_COUNT
git init -q .
git config commit.gpgsign true
git config extensions.worktreeConfig true
git config --worktree commit.gpgsign true
cp .git/config common.before
cp .git/config.worktree worktree.before
export GIT_CONFIG_COUNT=2
source "$1" 0
source "$1" 0
[[ "$(git config --get wrix.fixture)" == retained ]]
[[ "$(git config --show-scope --get commit.gpgsign)" == $'command\tfalse' ]]
git commit --allow-empty -qm unsigned
commit=$(git cat-file -p HEAD)
[[ "$commit" != *gpgsig* ]]
cmp common.before .git/config
cmp worktree.before .git/config.worktree
unset GIT_CONFIG_COUNT
[[ "$(git config --get commit.gpgsign)" == true ]]
"#,
    )
}

#[test]
fn sign_enabled_signs_without_deploy_or_shared_policy_changes() -> TestResult {
    let root = tempfile::tempdir()?;
    bootstrap(
        root.path(),
        r#"
set -euo pipefail
unset GIT_CONFIG_COUNT
git init -q .
git config commit.gpgsign false
cp .git/config common.before
ssh-keygen -q -t ed25519 -N '' -f "$HOME/signing"
export WRIX_SIGNING_KEY="$HOME/signing"
export WRIX_GIT_SIGN=0
export GIT_CONFIG_COUNT=2
source "$1" 1
[[ ! -v GIT_SSH_COMMAND ]]
[[ "$(git config --get wrix.fixture)" == retained ]]
[[ "$(git config --show-scope --get commit.gpgsign)" == $'command\ttrue' ]]
git commit --allow-empty -qm signed
commit=$(git cat-file -p HEAD)
[[ "$commit" == *gpgsig* ]]
git verify-commit HEAD
cmp common.before .git/config
unset GIT_CONFIG_COUNT
[[ "$(git config --get commit.gpgsign)" == false ]]
"#,
    )
}

#[test]
fn sign_enabled_requires_a_mounted_key() -> TestResult {
    let root = tempfile::tempdir()?;
    bootstrap(
        root.path(),
        r#"
set -euo pipefail
if (source "$1" 1); then exit 1; fi
"#,
    )
}

#[test]
fn bootstrap_rejects_invalid_effective_grant() -> TestResult {
    let root = tempfile::tempdir()?;
    bootstrap(
        root.path(),
        r#"
set -euo pipefail
if (source "$1" invalid); then exit 1; fi
"#,
    )
}
