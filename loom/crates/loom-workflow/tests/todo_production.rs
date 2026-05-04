//! Integration tests for [`ProductionTodoController`] that need a real git
//! repo. Pure logic for tier classification lives in
//! `src/todo/tier.rs::tests`; pure construction tests
//! (manifest lookup, template selection) live in
//! `src/todo/production.rs::tests`.
//!
//! These tests spawn the system `git` binary to seed and inspect a real
//! workspace (spec NFR #8): tier-1 fan-out and the per-spec cursor write
//! resolve through `LiveGitDiffSource` over `loom_core::git::GitClient`,
//! which only has anything to observe against real refs/index/diff state.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use loom_core::agent::SessionOutcome;
use loom_core::git::GitClient;
use loom_core::identifier::{MoleculeId, ProfileName, SpecLabel};
use loom_core::profile_manifest::ProfileImageManifest;
use loom_core::state::{ActiveMolecule, StateDb};
use loom_workflow::todo::{ProductionTodoController, TodoController, TodoError};

fn run_git(workspace: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .status()
        .expect("git spawn");
    assert!(status.success(), "git {args:?} failed: {status}");
}

fn capture_head(workspace: &Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse");
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn init_repo(workspace: &Path) -> Arc<GitClient> {
    run_git(workspace, &["init", "-q", "-b", "main"]);
    run_git(workspace, &["config", "user.email", "test@example.com"]);
    run_git(workspace, &["config", "user.name", "Test"]);
    run_git(workspace, &["config", "commit.gpgsign", "false"]);
    std::fs::write(workspace.join("seed.txt"), "seed\n").unwrap();
    run_git(workspace, &["add", "seed.txt"]);
    run_git(workspace, &["commit", "-q", "-m", "seed"]);
    Arc::new(GitClient::open(workspace).unwrap())
}

fn stub_manifest(dir: &Path) -> Arc<ProfileImageManifest> {
    let body = r#"{
      "base": { "ref": "localhost/wrapix-base:abc", "source": "/nix/store/aaa-image-base" }
    }"#;
    let path = dir.join("profile-images.json");
    std::fs::write(&path, body).unwrap();
    Arc::new(ProfileImageManifest::from_path(&path).unwrap())
}

fn empty_state(workspace: &Path) -> Arc<StateDb> {
    Arc::new(StateDb::open(workspace.join(".wrapix/loom/state.db")).unwrap())
}

fn seeded_state(
    workspace: &Path,
    label: &str,
    mol: &str,
    base_commit: Option<String>,
) -> Arc<StateDb> {
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(
        workspace.join(format!("specs/{label}.md")),
        format!("# {label}\n"),
    )
    .unwrap();
    let db = StateDb::open(workspace.join(".wrapix/loom/state.db")).unwrap();
    db.rebuild(
        workspace,
        &[ActiveMolecule {
            id: MoleculeId::new(mol),
            spec_label: SpecLabel::new(label),
            base_commit,
        }],
    )
    .unwrap();
    Arc::new(db)
}

#[tokio::test]
async fn build_spawn_config_resolves_manifest_image_and_renders_new_template() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_path_buf();
    let state = empty_state(&workspace);
    let manifest = stub_manifest(&workspace);
    let git = init_repo(&workspace);
    let mut ctrl = ProductionTodoController::new(
        SpecLabel::new("alpha"),
        workspace,
        state,
        manifest,
        ProfileName::new("base"),
        git,
        None,
    );
    let cfg = ctrl.build_spawn_config().await.expect("build cfg");
    assert_eq!(cfg.image_ref, "localhost/wrapix-base:abc");
    assert_eq!(
        cfg.image_source,
        std::path::PathBuf::from("/nix/store/aaa-image-base"),
    );
    assert!(
        cfg.initial_prompt.contains("Task Decomposition"),
        "TodoNewContext renders todo_new.md (header marker missing): {}",
        cfg.initial_prompt,
    );
    assert!(
        cfg.initial_prompt.contains("alpha"),
        "spec label must appear in rendered prompt: {}",
        cfg.initial_prompt,
    );
}

#[tokio::test]
async fn build_spawn_config_uses_update_template_when_molecule_exists() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_path_buf();
    let state = seeded_state(&workspace, "alpha", "wx-mol", None);
    let manifest = stub_manifest(&workspace);
    let git = init_repo(&workspace);
    let mut ctrl = ProductionTodoController::new(
        SpecLabel::new("alpha"),
        workspace,
        state,
        manifest,
        ProfileName::new("base"),
        git,
        None,
    );
    let cfg = ctrl.build_spawn_config().await.expect("build cfg");
    assert!(
        cfg.initial_prompt.contains("wx-mol"),
        "molecule id must thread into update template: {}",
        cfg.initial_prompt,
    );
}

#[tokio::test]
async fn build_spawn_config_surfaces_unknown_profile_as_profile_error() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_path_buf();
    let state = empty_state(&workspace);
    let manifest = stub_manifest(&workspace);
    let git = init_repo(&workspace);
    let mut ctrl = ProductionTodoController::new(
        SpecLabel::new("alpha"),
        workspace,
        state,
        manifest,
        ProfileName::new("missing"),
        git,
        None,
    );
    let err = ctrl
        .build_spawn_config()
        .await
        .expect_err("missing profile");
    assert!(
        matches!(err, TodoError::Profile(_)),
        "expected Profile, got {err:?}",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn build_spawn_config_tier_1_renders_diff_from_base_commit() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_path_buf();
    std::fs::create_dir_all(workspace.join("specs")).unwrap();
    std::fs::write(workspace.join("specs/alpha.md"), "# alpha\n").unwrap();
    let git = init_repo(&workspace);
    run_git(&workspace, &["add", "specs"]);
    run_git(&workspace, &["commit", "-q", "-m", "seed alpha"]);
    let base = capture_head(&workspace);

    std::fs::write(
        workspace.join("specs/alpha.md"),
        "# alpha\n\ntier-1 marker line\n",
    )
    .unwrap();
    run_git(&workspace, &["commit", "-q", "-am", "update alpha"]);

    let state = seeded_state(&workspace, "alpha", "wx-mol", Some(base));
    let manifest = stub_manifest(&workspace);
    let mut ctrl = ProductionTodoController::new(
        SpecLabel::new("alpha"),
        workspace,
        state,
        manifest,
        ProfileName::new("base"),
        git,
        None,
    );
    let cfg = ctrl.build_spawn_config().await.expect("build cfg");
    assert!(
        cfg.initial_prompt.contains("=== specs/alpha.md ==="),
        "tier-1 prompt must carry the per-spec diff header: {}",
        cfg.initial_prompt,
    );
    assert!(
        cfg.initial_prompt.contains("tier-1 marker line"),
        "tier-1 prompt must include the spec diff body: {}",
        cfg.initial_prompt,
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn record_outcome_advances_cursor_only_on_clean_exit() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_path_buf();
    let state = seeded_state(&workspace, "alpha", "wx-mol", None);
    let manifest = stub_manifest(&workspace);
    let git = init_repo(&workspace);
    let head_before = capture_head(&workspace);
    let label = SpecLabel::new("alpha");
    let mut ctrl = ProductionTodoController::new(
        label.clone(),
        workspace.clone(),
        Arc::clone(&state),
        manifest,
        ProfileName::new("base"),
        git,
        None,
    );

    ctrl.record_outcome(&SessionOutcome {
        exit_code: 1,
        cost_usd: None,
    })
    .await
    .expect("record outcome (failure)");
    assert_eq!(
        state.todo_cursor(&label).expect("cursor lookup"),
        None,
        "nonzero exit must NOT advance the cursor",
    );

    ctrl.record_outcome(&SessionOutcome {
        exit_code: 0,
        cost_usd: Some(0.42),
    })
    .await
    .expect("record outcome (success)");
    assert_eq!(
        state.todo_cursor(&label).expect("cursor lookup"),
        Some(head_before),
        "clean exit must record HEAD as the per-spec cursor",
    );
}
