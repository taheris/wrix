#!/usr/bin/env bash
set -euo pipefail

# Judge rubrics for beads.md success criteria

test_sync_in_container() {
  judge_files "lib/sandbox/linux/default.nix" "lib/sandbox/darwin/default.nix" "lib/sandbox/linux/entrypoint.sh" "lib/sandbox/darwin/entrypoint.sh"
  judge_criterion "Sandbox launchers start or reach the workspace service with 'wrix service start --no-cache', read the service-published Dolt endpoint through 'wrix service dolt ...', and pass BEADS_DOLT_SERVER_* into the container with BEADS_DOLT_AUTO_START disabled by the entrypoint. Linux uses the service Unix socket mounted into the sandbox; Darwin uses the service TCP host/port. A missing endpoint is fatal rather than silently falling back to a per-container Dolt or JSONL import."
}


test_shellhook_lifecycle_isolation() {
  judge_files "lib/beads/default.nix" "lib/default.nix"
  judge_criterion "On Linux, beads.shellHook starts the workspace service through the public 'wrix service start --no-cache' surface with a lifecycle independent of the caller process's cgroup. PASS if the Linux branch probes for user-systemd availability and, when available, wraps the wrix service start invocation in systemd-run --user --scope --collect; when user systemd is unavailable it falls back to a direct wrix service start. Both paths must propagate non-zero exit status as shellHook failure with no embedded-Dolt fallback. Darwin needs no equivalent wrapper because Apple's container and podman-via-VM already isolate via separate process trees."
}

test_shellhook_fail_loud() {
  judge_files "lib/beads/default.nix"
  judge_criterion "When a beads workspace is detected (\$PWD/.beads/dolt exists), beads.shellHook fails non-zero with a stderr message — and never falls back to embedded Dolt — in both fail-loud branches: (a) the missing-runtime branch, where Darwin requires Apple 'container' or podman (or an explicit WRIX_CONTAINER_RUNTIME) and Linux requires the selected service runtime, and (b) the unreachable-Dolt branch in waitAndExport, where the service-published endpoint from 'wrix service endpoints' must become reachable within the bounded startup budget before the loop gives up. Both branches must emit a stderr diagnostic identifying the failure cause and terminate the shellHook with non-zero status instead of leaving BEADS_DOLT_AUTO_START enabled."
}

test_workspace_naming_determinism() {
  judge_files "crates/wrix-core/src/path/mod.rs" "crates/wrix-service/src/lifecycle/mod.rs" "lib/beads/default.nix"
  judge_criterion "Workspace identity for beads comes from the services contract: the same service identity path yields the same '<repo>-service' container name, sha256-derived workspace hash, state/cache roots, preferred service ports, and Dolt endpoints across invocations, while different checkout identity paths do not collide. PASS if beads.shellHook delegates naming/endpoint selection to 'wrix service ...' and no longer carries legacy '<basename>-beads' helper logic; FAIL if WorkspaceHash is not a sha256 digest of the canonical service identity path, if port/root derivation uses a non-sha256 or basename-only identity, or if '_name' or equivalent basename(path)-beads helpers can satisfy the criterion."
}

test_beadspush_pushes_before_pulls() {
  judge_files "crates/wrix-beads/src/command/mod.rs"
  judge_criterion "On the happy path, wrix beads push invokes 'bd dolt push' before any 'bd dolt pull'. PASS if the dolt-sync sequence runs 'bd dolt commit' (best-effort) followed by 'bd dolt push' as the first remote-mutating call, and the 'bd dolt pull' invocation lives only inside a fallback branch that is entered when (and only when) the initial push exited non-zero. The fallback must itself only be triggered for fast-forward / non-fast-forward style rejections (e.g. by inspecting captured push stderr for 'non-fast-forward', 'reject', 'behind', 'out of date', or equivalent patterns); other push failures must propagate the original push exit code without ever calling 'bd dolt pull'. The net effect against an up-to-date remote is that 'bd dolt pull' is never executed, so the session-close run never enters the Dolt merge path."
}

test_beadspush_failloud_on_intent_overwrite() {
  judge_files "crates/wrix-beads/src/command/mod.rs"
  judge_criterion "On the pull-fallback path, wrix beads push snapshots local status and labels intent BEFORE running 'bd dolt pull', re-reads the same rows after the pull, and refuses to push when post-pull state diverges from the snapshot. PASS if all of the following hold: (a) the affected-issue set is derived from a Dolt system-table column diff between local HEAD and the remote-tracking branch (queries against dolt_commit_diff_issues and dolt_commit_diff_labels, or the equivalent dolt_diff_* per-table system tables, filtered to rows where status changed or labels were added/removed); (b) the pre-pull snapshot captures both status and the labels set for each affected issue (e.g. via SELECT against issues joined to a GROUP_CONCAT over labels, or equivalent); (c) the same snapshot query runs again after 'bd dolt pull' and the two outputs are compared (diff, equality check, or row-by-row comparison); (d) on any divergence, the script writes the affected issue IDs to stderr and exits non-zero WITHOUT calling 'bd dolt push' a second time; (e) the divergence check covers updates only — insert/delete row collisions are out of scope and need not be handled."
}

test_beadspush_disables_autoexport() {
  judge_files "crates/wrix-beads/src/command/mod.rs"
  judge_criterion "wrix beads push disables bd's auto-export hook on every invocation by running 'bd config set export.auto false' after workspace context is resolved and BEFORE any 'bd dolt' invocation, including Dolt remote inspection/repair and the first 'bd dolt commit' on the sync path. PASS if all of the following hold: (a) the implementation contains exactly one such write in the normal execution path; (b) no 'bd dolt' remote list, push, pull, or commit call can execute before it on the non-LOOM path; (c) the setting is intentionally persistent — there is no save/restore path and no later 'bd config set export.auto true', so the value persisted in .beads/config.yaml carries forward to subsequent bd calls inside and outside wrix beads push. FAIL if the write is missing, can be skipped on the happy path, is paired with a restore-to-true, or precedes the workspace setup it relies on."
}

test_beadspush_repairs_host_dolt_remote() {
  judge_files "crates/wrix-beads/src/command/mod.rs"
  judge_criterion "On non-LOOM invocations, wrix beads push ensures bd Dolt sync writes to the current checkout's beads worktree remote (file://\$ROOT/.git/beads-worktrees/\$BRANCH/.beads/dolt-remote). PASS if all of the following hold: (a) host invocations repair a missing or stale origin before any 'bd dolt commit/push/pull'; (b) sandbox/container invocations may temporarily replace origin for the sync, but restore the prior origin before exit so /workspace is not left in shared Beads config; (c) the handling inspects the remote named 'origin' specifically and does not treat a different remote that already points at the desired URL as proof that origin is correct; (d) it updates the SQL Dolt remote directly (for example via DOLT_REMOTE) rather than 'bd dolt remote add', so it cannot write or auto-commit sync.remote into .beads/config.yaml; (e) missing beads worktree remote is a no-op so bootstrap/recreate paths still work."
}

test_beadspush_loom_inside_noop() {
  judge_files "crates/wrix-beads/src/command/mod.rs"
  judge_criterion "When \$LOOM_INSIDE is set, wrix beads push performs a full no-op before any git or bd operation: main's first operation checks \$LOOM_INSIDE and, when non-empty, writes a one-line notice to stderr and exits 0. PASS if on the \$LOOM_INSIDE path no git command and no bd command (config, dolt commit/push/pull) can execute. FAIL if any bd/git call can run before the guard, if it does not exit 0, or if it is missing."
}

test_beadspush_failloud_missing_repo() {
  judge_files "crates/wrix-beads/src/command/mod.rs"
  judge_criterion "When \$LOOM_INSIDE is unset and 'git rev-parse --show-toplevel' does not resolve a workspace root, wrix beads push exits non-zero with an actionable stderr message naming the unresolved repository location — never proceeding with an empty root into a git invocation that prints 'fatal: not a git repository: (null)'. PASS if the context loader represents the unresolved root as an error/no-context state, the caller emits a one-line message that identifies the current directory and tells the operator to run inside a workspace checkout, and no worktree or git-branch sync path can run without a resolved root."
}

test_beadspush_pre_pull_cleanup_canonical() {
  judge_files "crates/wrix-beads/src/command/mod.rs"
  judge_criterion "The two leftover-dirt cleanup guards in wrix beads push — the one that runs before 'git pull --rebase' and the one that runs after the rsync of REMOTE_DIR — both detect a dirty beads worktree using the same surface 'git rebase' itself consults: 'git update-index --refresh' followed by 'git status --porcelain --untracked-files=normal' (a non-empty result triggers 'git add -A' + 'git commit'). PASS if BOTH guard sites use this canonical sequence and tolerate update-index refreshing stale stat entries. FAIL if either guard relies on the older 'git diff --quiet || git ls-files --others --exclude-standard' combination, which trusts the index's stat cache and can miss tracked-file content changes."
}

test_beadspush_recovers_orphaned_worktree() {
  judge_files "crates/wrix-beads/src/command/mod.rs"
  judge_criterion "The beads-branch sync recreates the beads worktree when its directory exists but is no longer a valid git worktree — the canonical orphaned case where '.git/worktrees/<branch>' was pruned or removed, leaving the directory present with a dangling gitdir pointer. PASS if the recreate guard fires on BOTH the absent directory case AND the present-but-invalid case via a validity probe, prunes stale worktree registrations, removes the invalid directory, and re-adds the worktree relative to the current resolved root before completing the sync. FAIL if the guard is absent-only, if the invalid case is not pruned/removed/re-added, or if a dangling worktree aborts the sync."
}

test_beadspush_worktree_recreate_skips_prek() {
  judge_files "crates/wrix-beads/src/command/mod.rs"
  judge_criterion "Every git invocation in the beads-branch sync section skips prek so the config-less 'beads' branch never aborts the sync with 'No prek.toml … found'. PASS if every git command runner used for worktree add, status, pull, commit, and push injects PREK_ALLOW_NO_CONFIG=1 without requiring the caller to pre-set it, while the bd/Dolt sync phase remains unchanged. FAIL if 'git worktree add' or later beads-branch git commands can run without the bypass."
}
