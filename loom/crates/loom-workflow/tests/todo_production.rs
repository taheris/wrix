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
use loom_workflow::todo::{ExitSignal, ProductionTodoController, TodoController, TodoError};

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

/// wx-hcolw.6 gate: `loom todo` must build a `SpawnConfig` whose
/// `initial_prompt` carries the rendered phase template body (with the
/// scratchpad path partial), whose `RePinContent` is an empty placeholder
/// — the rendered phase prompt now flows from `<scratch_dir>/prompt.txt`
/// via post-compaction `repin.sh`, not from the `repin` field — and whose
/// scratch dir holds a `prompt.txt` whose contents equal `initial_prompt`.
/// Mirror of the `loom check` and `loom run` dispatch-shape tests
/// (`src/check/production.rs`, `src/run/production.rs`).
#[tokio::test]
async fn build_session_dispatches_rendered_todo_template_and_writes_prompt_txt() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_path_buf();
    let state = empty_state(&workspace);
    let manifest = stub_manifest(&workspace);
    let git = init_repo(&workspace);
    let mut ctrl = ProductionTodoController::new(
        SpecLabel::new("loom-harness"),
        workspace,
        state,
        manifest,
        ProfileName::new("base"),
        git,
        None,
    );
    let session = ctrl.build_session().await.expect("build cfg");
    let cfg = &session.config;
    assert!(
        cfg.initial_prompt.contains("# Task Decomposition"),
        "prompt missing template heading: {}",
        cfg.initial_prompt,
    );
    assert!(
        cfg.initial_prompt.contains("specs/loom-harness.md"),
        "prompt missing spec path: {}",
        cfg.initial_prompt,
    );
    assert!(
        cfg.initial_prompt.contains(".wrapix/loom/scratch"),
        "prompt missing scratchpad partial: {}",
        cfg.initial_prompt,
    );
    assert!(
        cfg.repin.orientation.is_empty()
            && cfg.repin.pinned_context.is_empty()
            && cfg.repin.partial_bodies.is_empty(),
        "RePinContent must be empty placeholder; rendered template lives in prompt.txt: {:?}",
        cfg.repin,
    );
    let written =
        std::fs::read_to_string(cfg.scratch_dir.join("prompt.txt")).expect("prompt.txt readable");
    assert_eq!(written, cfg.initial_prompt);
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
    let session = ctrl.build_session().await.expect("build cfg");
    let cfg = &session.config;
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
    let session = ctrl.build_session().await.expect("build cfg");
    let cfg = &session.config;
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
    let err = match ctrl.build_session().await {
        Ok(_) => panic!("expected Profile error, got Ok"),
        Err(e) => e,
    };
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
    let session = ctrl.build_session().await.expect("build cfg");
    let cfg = &session.config;
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

#[tokio::test]
async fn build_spawn_config_renders_implementation_notes_from_db() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_path_buf();
    let state = seeded_state(&workspace, "alpha", "wx-mol", None);
    state
        .set_implementation_notes(
            &SpecLabel::new("alpha"),
            &[
                "carry over: touch lib/foo".into(),
                "watch out for the wrapix env contract".into(),
            ],
        )
        .expect("seed notes");
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

    let session = ctrl.build_session().await.expect("build cfg");
    let prompt = &session.config.initial_prompt;
    assert!(
        prompt.contains("## Implementation Notes"),
        "todo prompt must render the section header when notes are present: {prompt}",
    );
    assert!(
        prompt.contains("- carry over: touch lib/foo"),
        "first note must be rendered as a bullet: {prompt}",
    );
    assert!(
        prompt.contains("- watch out for the wrapix env contract"),
        "second note must be rendered as a bullet: {prompt}",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn record_outcome_clears_notes_on_success_but_not_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().to_path_buf();
    let state = seeded_state(&workspace, "alpha", "wx-mol", None);
    let label = SpecLabel::new("alpha");
    state
        .set_implementation_notes(&label, &["one".into(), "two".into()])
        .expect("seed notes");
    let manifest = stub_manifest(&workspace);
    let git = init_repo(&workspace);
    let mut ctrl = ProductionTodoController::new(
        label.clone(),
        workspace.clone(),
        Arc::clone(&state),
        manifest,
        ProfileName::new("base"),
        git,
        None,
    );

    ctrl.record_outcome(
        &SessionOutcome {
            exit_code: 1,
            cost_usd: None,
        },
        Some(&ExitSignal::Complete),
    )
    .await
    .expect("record outcome (nonzero exit)");
    assert_eq!(
        state.spec(&label).expect("spec row").implementation_notes,
        Some(vec!["one".to_string(), "two".to_string()]),
        "nonzero exit must NOT clear notes — gate matches the cursor gate",
    );

    ctrl.record_outcome(
        &SessionOutcome {
            exit_code: 0,
            cost_usd: None,
        },
        None,
    )
    .await
    .expect("record outcome (missing marker)");
    assert_eq!(
        state.spec(&label).expect("spec row").implementation_notes,
        Some(vec!["one".to_string(), "two".to_string()]),
        "missing marker must NOT clear notes — gate matches the cursor gate",
    );

    ctrl.record_outcome(
        &SessionOutcome {
            exit_code: 0,
            cost_usd: None,
        },
        Some(&ExitSignal::Complete),
    )
    .await
    .expect("record outcome (success)");
    let row = state.spec(&label).expect("spec row");
    assert!(
        row.implementation_notes.is_none(),
        "complete marker + clean exit must drive notes back to NULL: {:?}",
        row.implementation_notes,
    );
    // Row itself must persist — molecules and companions reference it.
    assert_eq!(row.label, label, "spec row must remain after notes clear");
}

#[tokio::test(flavor = "multi_thread")]
async fn record_outcome_advances_cursor_only_on_complete_marker_and_clean_exit() {
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

    ctrl.record_outcome(
        &SessionOutcome {
            exit_code: 1,
            cost_usd: None,
        },
        Some(&ExitSignal::Complete),
    )
    .await
    .expect("record outcome (failure)");
    assert_eq!(
        state.todo_cursor(&label).expect("cursor lookup"),
        None,
        "nonzero exit must NOT advance the cursor",
    );

    ctrl.record_outcome(
        &SessionOutcome {
            exit_code: 0,
            cost_usd: Some(0.42),
        },
        None,
    )
    .await
    .expect("record outcome (missing marker)");
    assert_eq!(
        state.todo_cursor(&label).expect("cursor lookup"),
        None,
        "missing marker must NOT advance the cursor even on exit 0",
    );

    ctrl.record_outcome(
        &SessionOutcome {
            exit_code: 0,
            cost_usd: Some(0.42),
        },
        Some(&ExitSignal::Complete),
    )
    .await
    .expect("record outcome (success)");
    assert_eq!(
        state.todo_cursor(&label).expect("cursor lookup"),
        Some(head_before),
        "complete marker + clean exit must record HEAD as the per-spec cursor",
    );
}
