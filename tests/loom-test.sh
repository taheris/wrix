#!/usr/bin/env bash
# Verify tests for the Loom harness spec.
#
# Each function exits 0 on PASS, non-zero on FAIL, 77 to skip.
# Invoked by `ralph spec --verify` as `tests/loom-test.sh <function_name>`.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOOM_DIR="$REPO_ROOT/loom"

# Run cargo with a Rust toolchain. Inside the devshell, cargo is on PATH
# (fenix-provided rustc 1.95). Outside, fall back to `nix develop` so the
# workspace's pinned toolchain is used — nixpkgs' rustc lags the workspace's
# `rust-version = "1.95"` requirement.
cargo_run() {
    if command -v cargo >/dev/null 2>&1; then
        ( cd "$LOOM_DIR" && cargo "$@" )
    else
        nix develop "$REPO_ROOT" --command bash -c "cd '$LOOM_DIR' && cargo $*"
    fi
}

#-----------------------------------------------------------------------------
# Pending-implementation stub helper. Defined at the top of the file (above
# all real test_* dispatchers) so the `loom doctor` body-capture heuristic
# in `loom-workflow/src/doctor.rs::parse_dispatcher` — which reads forward
# from each `test_<name>()` header until the next `test_<name>()` header —
# cannot confuse this helper's textual occurrence with a real dispatcher
# that has been promoted to `[x]` in the spec. Stub dispatchers call this
# helper to mark themselves "not implemented yet" by exiting 77 (the
# de-facto POSIX "skipped" code, also used by Automake test runners).
# Promotion path: when the implementation lands, replace the stub body
# with a real dispatcher into the corresponding Rust test (drop the
# `_pending_stub` call). The review gate (specs/loom-harness.md
# §"Stub-to-real review gate") rejects PRs that land implementation
# work while the matching stub is still here.
#-----------------------------------------------------------------------------
_pending_stub() {
    echo "test_${1}: pending implementation — see specs/loom-harness.md §Stub-to-real review gate" >&2
    exit 77
}

#-----------------------------------------------------------------------------
# test_workspace_builds — `cargo build` from loom/ root succeeds.

#-----------------------------------------------------------------------------
# test_nix_build — `nix build .#loom` succeeds and produces a loom binary.

#-----------------------------------------------------------------------------
# test_workspace_clippy_lints — loom-tests spec Functional #6: clippy block
# denies unwrap_used, expect_used, panic, todo, unimplemented; warns
# allow_attributes (use `#[expect(...)]` over `#[allow(...)]`).
#-----------------------------------------------------------------------------
#-----------------------------------------------------------------------------
# Helpers for wrapix spawn acceptance tests.
#
# The wrapix script has a WRAPIX_DRY_RUN=1 mode that parses the SpawnConfig
# and prints resolved spawn state without invoking the container runtime.
# That lets us verify SpawnConfig consumption on any host (incl. CI without
# /dev/kvm) without booting a real container.
#-----------------------------------------------------------------------------
wrapix_bin() {
    nix build "$REPO_ROOT#sandbox" --no-link --print-out-paths 2>/dev/null
}

write_spawn_config() {
    local path="$1" image_ref="$2" image_source="${3:-/nix/store/zzz-wrapix-test.tar}" workspace="${4:-/some/workspace}"
    cat >"$path" <<EOF
{
  "image_ref": "$image_ref",
  "image_source": "$image_source",
  "workspace": "$workspace",
  "env": [["WRAPIX_AGENT","claude-code"],["TERM","dumb"]],
  "initial_prompt": "do the thing",
  "agent_args": ["--print","--output-format","stream-json"],
  "repin": {"orientation":"o","pinned_context":"pc","partial_bodies":{}}
}
EOF
}

#-----------------------------------------------------------------------------
# test_wrapix_spawn_subcommand — `wrapix spawn --spawn-config <f> --stdio`
# parses the JSON, omits TTY (STDIO=1), and exposes the resolved spawn state.
# Verified through the launcher's WRAPIX_DRY_RUN=1 mode so no container
# runtime is required.

#-----------------------------------------------------------------------------
# test_spawn_config_json_stability — every documented SpawnConfig field
# round-trips through the JSON shape without rename or loss.

#-----------------------------------------------------------------------------
# test_per_bead_profile_spawn — two beads with different `profile:X` labels
# resolve through `build_spawn_config_from_manifest` to two SpawnConfigs with
# different `image_ref` + `image_source`. Verified by the unit test in
# `loom-workflow::run::spawn::tests`. The integration test in
# `loom/tests/spawn_dispatch.rs` records the argv shape (`spawn
# --spawn-config <file> --stdio`) end-to-end against a wrapix shim.

#-----------------------------------------------------------------------------
# test_profiles_manifest_required — loom reads `LOOM_PROFILES_MANIFEST` at
# startup and parses it into `BTreeMap<ProfileName, ImageEntry>`; missing
# env var or missing file errors before any bead spawn (no implicit search
# path or fallback default). The two paths (env unset, file missing) are
# unit-tested under `loom-driver::profile_manifest::manifest::tests`.

#-----------------------------------------------------------------------------
# test_unknown_profile_errors — a bead with `profile:X` where `X` is not
# in the manifest fails with a typed `ProfileError::UnknownProfile` naming
# the missing profile. Verified by the lookup unit test under
# `loom-driver::profile_manifest::manifest::tests`.

#-----------------------------------------------------------------------------
# test_profile_cli_override — `--profile` CLI override takes precedence
# over bead labels. The override flows through `loom run` → `dispatch_for_slot`
# → `build_spawn_config_from_manifest`; the unit test in
# `loom-workflow::run::spawn::tests::cli_override_swaps_resolved_image`
# pins the resolution surface, while
# `loom-workflow::run::profile::tests::resolve_profile_image_cli_override_wins_over_label`
# pins the precedence chain (CLI > label > phase default).


#-----------------------------------------------------------------------------
# Workflow commands — each function dispatches into the matching cargo unit
# test under `loom-workflow`. Sharing the cargo binary keeps verify and
# `cargo test` exercising the same code paths.
#-----------------------------------------------------------------------------
todo_cargo_test() {
    cargo_run test -p loom-workflow --lib "$1" -- --exact --nocapture --quiet
}

todo_production_cargo_test() {
    cargo_run test -p loom-workflow --test todo_production "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_todo_tier_detection — `compute_spec_diff` correctly classifies inputs
# into the four tiers from `lib/ralph/cmd/util.sh::compute_spec_diff`:
#   - Tier 1 (diff): molecule + valid base_commit → per-spec fan-out, with
#     sibling overrides and orphan fallback to the anchor's base.
#   - Tier 2 (tasks): molecule present but base_commit absent / orphaned /
#     missing → fall back to existing-task comparison.
#   - Tier 4 (new): no molecule recorded → fresh decomposition.
#   - --since override: replaces the anchor's base for the anchor only;
#     errors when the override commit is missing or orphaned.
# (Tier 3 — README discovery — is the driver's responsibility before
# `compute_spec_diff` runs; once it reconstructs a molecule, the call
# proceeds as tier 2.)

#-----------------------------------------------------------------------------
# State database — each function dispatches into the matching cargo
# integration test under `loom-driver/tests/state_db.rs`. Sharing the cargo
# binary keeps verify and `cargo test` exercising the same code paths.
#-----------------------------------------------------------------------------
state_db_cargo_test() {
    cargo_run test -p loom-driver --test state_db "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_state_db_init — `StateDb::open` creates `specs`, `molecules`,
# `companions`, and `meta` tables and seeds `meta.schema_version`.

#-----------------------------------------------------------------------------
# test_state_db_rebuild — `StateDb::rebuild` writes one specs row per
# spec markdown file and one molecules row per active molecule.

#-----------------------------------------------------------------------------
# test_state_db_rebuild_companions — `## Companions` parser extracts paths,
# specs without the section contribute zero rows, malformed lines skip.

#-----------------------------------------------------------------------------
# test_state_db_rebuild_resets_counters — `iteration_count` returns to 0
# after rebuild, even when previously incremented.

#-----------------------------------------------------------------------------
# test_state_current_spec — `set_current_spec` followed by `current_spec`
# round-trips the same `SpecLabel`.

#-----------------------------------------------------------------------------
# test_state_increment_iteration — `increment_iteration` returns the post-
# increment value (1, 2, 3, ...).

#-----------------------------------------------------------------------------
# test_state_corruption_recovery — opening a non-SQLite blob fails; the
# `recreate()` recovery path replaces the file and rebuild succeeds.

#-----------------------------------------------------------------------------
# test_state_todo_cursor — `set_todo_cursor` / `todo_cursor` round-trip a
# per-spec commit SHA through the `meta` table; subsequent writes overwrite
# (cursor moves forward as `loom todo` runs); per-spec namespacing keeps
# distinct labels disjoint.

#-----------------------------------------------------------------------------
# Beads CLI wrapper — each function dispatches into a unit test under
# loom-driver/src/bd/client.rs::tests, so verify and `cargo test` exercise the
# same code paths. Tests substitute a `CapturingRunner` to keep the verify
# path independent of a real `bd` binary.
#-----------------------------------------------------------------------------
bd_client_cargo_test() {
    cargo_run test -p loom-driver --lib "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_bd_show_parsing — `bd show <id> --json` output deserializes into a
# typed `Bead` value with the expected fields.

#-----------------------------------------------------------------------------
# test_bd_list_parsing — `bd list --json` output deserializes into a Vec<Bead>
# and the `--status=` / `--label=` filters are forwarded to the CLI argv.

#-----------------------------------------------------------------------------
# test_bd_create_returns_id — `bd create --silent` stdout (a single id) is
# parsed back into a `BeadId`; blank stdout maps to `BdError::CreateMissingId`.

#-----------------------------------------------------------------------------
# test_bd_error_handling — non-zero exits map to `BdError::Cli` with the
# argv + stderr captured; malformed JSON maps to `BdError::Decode`; missing
# rows map to `BdError::ShowEmpty`.

#-----------------------------------------------------------------------------
# Run-time logging — each function dispatches into a cargo integration test
# under `loom-driver/tests/logging.rs`. Sharing the cargo binary keeps verify
# and `cargo test` exercising the same code paths.
#-----------------------------------------------------------------------------
logging_cargo_test() {
    cargo_run test -p loom-driver --test logging "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_run_default_output_shape — default render mode prints one header line
# per bead and one short line per tool call (assistant text deltas
# suppressed); the closing line carries tool count + duration.

#-----------------------------------------------------------------------------
# test_run_verbose_streams_text — verbose mode streams MessageDelta verbatim.

#-----------------------------------------------------------------------------
# test_run_writes_per_bead_jsonl_log — every bead spawn writes the full
# AgentEvent stream as JSONL to
# `<workspace>/.wrapix/loom/logs/<spec>/<bead>-<utc>.jsonl`, regardless of
# terminal verbosity.

#-----------------------------------------------------------------------------
# test_run_logs_log_path — opening a sink emits an info-level tracing event
# whose `log_path` field carries the resolved file path.

#-----------------------------------------------------------------------------
# test_parallel_logs_are_per_bead — running two beads against the same logs
# root writes two distinct files (per-bead, not per-session), and the
# contents never cross-contaminate even when `emit` is interleaved.

#-----------------------------------------------------------------------------
# test_log_retention_sweep — `sweep_retention_at` deletes files older than
# `[logs] retention_days` and preserves recent files.

#-----------------------------------------------------------------------------
# test_log_retention_disabled — `retention_days = 0` disables sweeping.

#-----------------------------------------------------------------------------
# test_log_retention_failure_tolerance — per-file delete failures (here: a
# read-only directory) do not abort the sweep; survivors and failures are
# both surfaced in the report.

#-----------------------------------------------------------------------------
# Concurrency & locking — each function dispatches into a cargo integration
# test under `loom-driver/tests/lock_manager.rs` so verify and `cargo test`
# exercise the same paths. The acceptance behaviour (per-spec serialization,
# 5s timeout, cross-spec independence, read-only commands unblocked,
# init/workspace exclusion, crash recovery) is asserted in those tests.
#-----------------------------------------------------------------------------
lock_cargo_test() {
    cargo_run test -p loom-driver --test lock_manager "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_per_spec_lock_acquired — `LockManager::acquire_spec` creates the per-
# spec lock file and the guard releases on drop.

#-----------------------------------------------------------------------------
# test_per_spec_lock_serializes — a second mutating command on the same spec
# waits up to 5s and then errors with `another loom command is operating on
# <label>`. The fast contention path (250ms timeout) and the default 5s wait
# are both asserted.

#-----------------------------------------------------------------------------
# test_cross_spec_no_blocking — locks for distinct spec labels do not block
# each other; both acquire effectively immediately.

#-----------------------------------------------------------------------------
# test_readonly_commands_unblocked — read-only commands acquire no lock; an
# active spec lock does not block read-only inspection of the workspace.

#-----------------------------------------------------------------------------
# test_init_workspace_lock — `acquire_workspace` errors immediately with
# `WorkspaceBusy` if any per-spec lock is held; succeeds when none are; and
# is exclusive against itself.

#-----------------------------------------------------------------------------
# test_crash_releases_lock — a crashed (process-exit) holder leaves no stale
# lock; a fresh invocation acquires immediately. The integration test spawns
# the cargo test binary as a child process, takes the lock, then exits via
# `std::process::exit` so the kernel — not Rust's Drop — releases the flock.

#-----------------------------------------------------------------------------
# loom run — each function dispatches into a cargo unit test under
# `loom-workflow/src/run/`. Sharing the cargo binary keeps verify and
# `cargo test` exercising the same code paths. The driver is exercised via
# the `AgentLoopController` trait so the tests never need a real container,
# bd binary, or `loom review` exec.
#-----------------------------------------------------------------------------
run_cargo_test() {
    cargo_run test -p loom-workflow --lib "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_run_continuous — continuous mode pulls beads until `next_ready_bead`
# returns `None`, closes each on success, and execs `loom review` exactly once
# at molecule completion.

#-----------------------------------------------------------------------------
# test_run_once — `--once` processes a single bead then returns; subsequent
# ready beads remain in the queue and `loom review` is never invoked.

#-----------------------------------------------------------------------------
# test_run_profile_selection — `resolve_profile` reads the bead's `profile:X`
# label, falls back to the phase default without a label, and honours the
# CLI override.

#-----------------------------------------------------------------------------
# test_run_retry_with_context — a failing bead retries with `previous_failure`
# threaded into the next attempt, gives up after `max_retries`, and the
# RetryPolicy decision math is asserted directly.

#-----------------------------------------------------------------------------
# Worktree parallelism — `--parallel N`. Pure-logic tests live in the
# loom-workflow lib; tests that touch a real git repo live in the
# `parallel` integration test.
#-----------------------------------------------------------------------------
parallel_lib_test() {
    cargo_run test -p loom-workflow --lib "$1" -- --exact --nocapture --quiet
}
parallel_int_test() {
    cargo_run test -p loom-workflow --test parallel "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_run_parallel_flag_validation — `Parallelism::from_str` accepts positive
# integers and rejects 0, negatives, and non-integers with a clear error
# (before any work begins).

#-----------------------------------------------------------------------------
# test_parallel_one_no_worktree — `--parallel 1` (default) does not create
# a worktree and works on the driver branch directly. The dispatch predicate
# is `Parallelism::is_one()`; the integration test pins it.

#-----------------------------------------------------------------------------
# test_parallel_creates_worktrees — `--parallel N > 1` creates one worktree
# per dispatched bead under `.wrapix/worktree/<label>/<bead-id>/` on a fresh
# branch `loom/<label>/<bead-id>` based on HEAD.

#-----------------------------------------------------------------------------
# test_parallel_concurrent_spawns — `run_concurrent_spawns` joins futures via
# `tokio::JoinSet` so wall-clock time for N concurrent dispatch slots is
# dominated by a single slot's work, not the sum.

#-----------------------------------------------------------------------------
# test_parallel_merge_back — successful bead branches are merged back to the
# driver branch sequentially after the batch completes; the per-bead worktree
# directory and branch are reclaimed on a clean merge.

#-----------------------------------------------------------------------------
# test_parallel_failure_cleanup — on agent failure the worktree branch is
# deleted and the bead is queued for retry per the retry policy
# (`BatchResult::AgentFailed` carries the error body the driver threads
# back into the next attempt as `previous_failure`).

#-----------------------------------------------------------------------------
# test_parallel_conflict_preserves_worktree — on merge conflict the worktree
# is preserved (not silently overwritten) and the bead is marked failed via
# `BatchResult::Conflict`. The branch is not deleted; the path on disk
# remains for human inspection.

#-----------------------------------------------------------------------------
# Note: the compiled-binary smoke for `loom run --once` lives at
# `loom/crates/loom/tests/run_smoke.rs`; it runs via `cargo nextest run
# --workspace` (and therefore under `nix flake check`). It does not
# carry a verify-runner wrapper because no spec acceptance criterion
# scopes it — it's a defensive regression test for the clap surface,
# not a contract surface. If a spec gains a binary-smoke acceptance,
# add a wrapper here that shells to it.
#-----------------------------------------------------------------------------

#-----------------------------------------------------------------------------
# loom review / loom msg — same dispatch pattern as run: each function pins one
# or more pure unit tests under `loom-workflow::{review,msg}`. The push-gate /
# auto-iterate decision logic is exercised through the `ReviewController` trait
# under a `FakeController`, so no real container, bd binary, or `git push` is
# spawned by these tests.
#-----------------------------------------------------------------------------
check_cargo_test() {
    cargo_run test -p loom-workflow --lib "$1" -- --exact --nocapture --quiet
}

msg_cargo_test() {
    cargo_run test -p loom-workflow --lib "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_check_push_gate — clean review (no new beads, no clarify) pushes once
# and resets the iteration counter; a clarify present (new or pre-existing)
# stops the gate without pushing.

#-----------------------------------------------------------------------------
# test_check_auto_iterate — fix-up beads under the iteration cap trigger an
# `exec loom run` with the counter incremented; reaching the cap escalates the
# newest fix-up bead to `ralph:clarify` instead of looping forever.

#-----------------------------------------------------------------------------
# test_msg_list — message list filters to `loom:clarify`- and `loom:blocked`-
# labelled beads, drops the SPEC column under a spec filter, and falls back to
# bead title when the `## Options — <summary>` header is missing.

#-----------------------------------------------------------------------------
# test_msg_fast_reply — `-a <choice>` resolves a pure-integer to the matching
# `### Option <N>` per the Options Format Contract for clarify beads; a
# missing index errors with the available indices; non-integer choice is
# stored verbatim. Blocked beads always store verbatim (free-form).

#-----------------------------------------------------------------------------
# Auxiliary commands (init, status, use, logs, spec) — each function dispatches
# into a unit test under loom-workflow/src/. Sharing the cargo binary keeps the
# verify path and `cargo test` exercising the same code paths.
#-----------------------------------------------------------------------------
aux_cargo_test() {
    cargo_run test -p loom-workflow --lib "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_init_creates_state — `loom init` creates `.wrapix/loom/config.toml`
# (round-trips through `LoomConfig::default()`) and `.wrapix/loom/state.db`,
# preserving an existing config file on subsequent invocations.

#-----------------------------------------------------------------------------
# test_init_rebuild — `loom init --rebuild` drops and repopulates the state
# DB from `specs/*.md` plus the supplied molecule slice, and resets every
# `iteration_count` to 0.

#-----------------------------------------------------------------------------
# test_status_command — `loom status` (read-only) renders `<unset>` when no
# spec has been chosen, otherwise prints the active spec, molecule id, and
# iteration counter. Sanity check confirms the call needs no lock.

#-----------------------------------------------------------------------------
# test_use_command — `loom use <label>` acquires the per-spec lock, writes
# `current_spec` to the state DB, and round-trips with `status::load`. A
# spec lock held elsewhere causes `SpecBusy` after the configured timeout.

#-----------------------------------------------------------------------------
# test_logs_command — `loom logs` (read-only) walks `.wrapix/loom/logs/` two
# levels deep, returns the most recent `*.jsonl`, applies an exact bead-id
# prefix filter so `wx-1` does not collapse into `wx-10`, and rejects
# non-jsonl files.

#-----------------------------------------------------------------------------
# test_spec_query — `loom spec` parses `## Success Criteria` checkboxes and
# pairs each with the following `[verify](path#fn)` / `[judge](path#fn)`
# annotation. Fenced code blocks, the next `##` heading, and orphan
# checkboxes (no annotation) are all handled per `parse_spec_annotations` in
# `lib/ralph/cmd/util.sh`.

#-----------------------------------------------------------------------------
# test_spec_deps — `loom spec --deps` mirrors `ralph sync --deps`: it scans
# every `[verify]`/`[judge]` test file for known tool invocations (curl, jq,
# rg, tmux, ssh, etc.), collapses aliases (`rg`/`ripgrep`, `ssh`/`scp`) to a
# single nixpkgs name, and ignores substring matches such as "curling".

#-----------------------------------------------------------------------------
# test_plan_new — `loom plan -n <label>` renders the new-spec template, shells
# out to interactive `wrapix run` (NOT `spawn --stdio`), waits for the
# session to exit, then re-parses `## Companions` from the spec markdown the
# interview wrote and replaces the companion rows for `<label>` in state.db.

#-----------------------------------------------------------------------------
# test_plan_update — `loom plan -u <label>` requires the spec to already
# exist, threads the existing companion rows into the update template, and
# reconciles companions from the spec markdown after the interactive session
# exits.

#-----------------------------------------------------------------------------
# test_plan_uses_interactive_wrapix_run — `loom plan` must shell out to the
# interactive `wrapix run` subcommand with the user's TTY attached. It must
# NEVER use `wrapix spawn`, NEVER pass `--stdio`, and NEVER pass
# `--spawn-config` — those are reserved for the JSONL-driven phases. The
# launcher (lib/sandbox/linux/default.nix) refuses `wrapix run` without
# `WRAPIX_DEFAULT_IMAGE_REF` / `WRAPIX_DEFAULT_IMAGE_SOURCE`, so plan must
# resolve its profile against the parsed manifest and inject those env vars
# into the child env before exec'ing.

#-----------------------------------------------------------------------------
# Agent backend trait surface — pin the loom-driver types and modules that
# loom-agent depends on. Each grep test lives next to the file under test so
# the failure message points directly at the source.
#-----------------------------------------------------------------------------


#-----------------------------------------------------------------------------
# Agent runtime layer — verifies the two-axis composition in
# lib/sandbox/image.nix. The pi runtime layer adds Node.js + pi binary on top
# of any workspace profile; the claude runtime layer is a no-op (claude is
# already in the base image).
#
# Tests inspect the realized image closure rather than booting a container,
# so they run on any host that can build a Linux image (Linux directly,
# Darwin via the remote linux-builder).
#-----------------------------------------------------------------------------

# Build an image and emit its full closure (one store path per line).
image_closure() {
    local attr="$1" out
    out=$(nix build "$REPO_ROOT#$attr" --no-link --print-out-paths) || return 1
    nix-store --query --requisites "$out"
}


#-----------------------------------------------------------------------------
# test_pi_binary_in_container — the pi binary the runtime layer bundles is
# functional: `pi --version` succeeds. Exercised against the pi-mono store
# path that the layer pulls in.
#-----------------------------------------------------------------------------
test_pi_binary_in_container() {
    local pi_pkg
    pi_pkg=$(nix build "$REPO_ROOT#pi-mono" --no-link --print-out-paths) || return 1
    [ -x "$pi_pkg/bin/pi" ] \
        || { echo "pi binary missing at $pi_pkg/bin/pi" >&2; return 1; }
    "$pi_pkg/bin/pi" --version >/dev/null \
        || { echo "'pi --version' failed against $pi_pkg/bin/pi" >&2; return 1; }
    # The same pi-mono store path must appear in a pi-runtime image, otherwise
    # the layer is referencing a different binary than the one we just probed.
    local closure
    closure=$(image_closure sandbox-pi) || return 1
    grep -qF "$pi_pkg" <<<"$closure" \
        || { echo "pi binary $pi_pkg not in sandbox-pi closure" >&2; return 1; }
}

#-----------------------------------------------------------------------------
# test_claude_runtime_noop — the default (claude) runtime adds nothing on top
# of the base image: pi-mono is absent, and claude itself is still present
# (sanity check that the base image's claude binary survived the refactor).
#-----------------------------------------------------------------------------
test_claude_runtime_noop() {
    local closure
    closure=$(image_closure sandbox) || return 1
    if grep -q -- '-pi-mono-' <<<"$closure"; then
        echo "pi-mono unexpectedly present in claude-runtime sandbox closure" >&2
        return 1
    fi
    grep -q -- '-claude-code-' <<<"$closure" \
        || { echo "claude-code missing from default sandbox closure (regression)" >&2; return 1; }
}

#-----------------------------------------------------------------------------
# test_protocol_versions_pinned — both pi-mono and claude-code are pinned
# in modules/flake/overlays.nix with documentation. Per spec NFR #9:
# version bumps go through a checklist; the pin file is the single
# discoverable point reviewers grep when bumping either dependency.
#-----------------------------------------------------------------------------
#-----------------------------------------------------------------------------
# test_flake_check_includes_loom — `nix flake check` runs both Rust gates
# from the rust profile's buildPackage outputs. Asserts
# `checks.<current-system>.loom-clippy` and `checks.<current-system>.loom-nextest`
# evaluate to store paths from the matching crane derivations. This is the
# gate that binds the unit + integration tier to CI.
#-----------------------------------------------------------------------------
#-----------------------------------------------------------------------------
# test_flake_declares_loom_for_all_systems — per spec NFR #7 the unit +
# integration tier is cross-platform. Asserts checks.<system>.loom-clippy
# and checks.<system>.loom-nextest evaluate for all four supported systems.
#-----------------------------------------------------------------------------
#-----------------------------------------------------------------------------
# test_newtype_serde_roundtrip — every domain identifier newtype
# (`BeadId`, `SpecLabel`, `MoleculeId`, `ProfileName`, `SessionId`,
# `ToolCallId`, `RequestId`) round-trips through serde JSON without a
# wrapper (`#[serde(transparent)]`) and rejects malformed input. Exercises
# the inline `#[cfg(test)] mod tests` blocks in
# `loom-events/src/identifier/*.rs` (re-exported via
# `loom_driver::identifier`). Each module pins the five parse-boundary
# tests: `serde_round_trips_as_plain_string`,
# `deserialize_rejects_malformed_string`, `display_round_trips_with_as_str`,
# `parse_accepts_canonical_shapes`, `parse_rejects_malformed_inputs`.
#-----------------------------------------------------------------------------
#-----------------------------------------------------------------------------
# test_state_db_roundtrip — `StateDb` covers spec, molecule, companions, and
# meta-table operations end-to-end: open creates schema, rebuild populates
# from `specs/*.md` + mock molecule list, `current_spec` round-trips,
# `increment_iteration` returns the post-increment value (starting at 0
# after rebuild), and `recreate` recovers from a corrupted file. Each cargo
# integration test exercises one operation against a `tempfile::tempdir`.
#-----------------------------------------------------------------------------
#-----------------------------------------------------------------------------
# test_pi_protocol_coverage — Pi RPC parser covers every documented field of
# every documented message type for the pinned protocol version (per spec
# NFR #9): two-phase deserialization, response success/failure with
# `data` / `error` field mapping, `tool_execution_start` / `_end` field
# mapping (`toolCallId` / `toolName` / `args` / `result` / `isError`),
# `message_update` nested delta dispatch (text, error, unmapped),
# compaction lifecycle reasons, observability-only events, malformed JSON,
# extension UI auto-cancel, and command (prompt/steer/abort) serialization.
#-----------------------------------------------------------------------------
#-----------------------------------------------------------------------------
# test_claude_protocol_coverage — Claude stream-json parser covers every
# documented field of every documented message type:
# `#[serde(tag = "type")]` dispatch, `#[serde(other)]` → `Unknown` for
# forward compatibility, `Result` with all six fields (`subtype`, `result`,
# `total_cost_usd`, `duration_ms`, `num_turns`, `is_error`), `System` /
# `ControlRequest` / assistant / user block field mappings, control_request
# auto-approval keyed on the deny-list, and malformed-JSON handling.
#-----------------------------------------------------------------------------
#-----------------------------------------------------------------------------
# test_template_rendering — every Askama template renders cleanly with
# representative inputs. Exercises the integration tests under
# `loom-templates/tests/render.rs`, which assert on shared sections,
# partials, agent-output wrapping, and `previous_failure` truncation.
#-----------------------------------------------------------------------------
#-----------------------------------------------------------------------------
# Integration tests — load-bearing flows from spec §Functional #4. Each
# top-level wrapper corresponds to one acceptance criterion in
# specs/loom-tests.md §Integration tests; the underlying cargo test names
# are free to evolve, but the shell function names are pinned by the
# annotation gate.
#-----------------------------------------------------------------------------

#-----------------------------------------------------------------------------
# test_startup_probe_roundtrip — mock pi with the full required command set
# lets loom proceed; mock pi missing `set_model` causes loom to fail fast
# with a version-mismatch error. Exercises the integration tests under
# `loom-agent/tests/static_dispatch.rs` driving the real mock-pi over a
# real pipe.
#-----------------------------------------------------------------------------
#-----------------------------------------------------------------------------
# test_wrapix_spawn_argv_contract — loom invokes
# `wrapix spawn --spawn-config <file> --stdio` with stdin attached as a
# pipe (not a TTY); the recorded `SpawnConfig` JSON matches the on-disk
# shape (with `image_ref` + `image_source` fields). The argv-shape +
# pipe-not-tty contract are both covered by the integration tests in
# `loom/tests/spawn_dispatch.rs`.
#-----------------------------------------------------------------------------
#-----------------------------------------------------------------------------
# test_parallel_run_end_to_end — `loom run --parallel 2` with two ready
# beads dispatches two mock-agent spawns concurrently, each in its own
# worktree under `.wrapix/worktree/<label>/<bead-id>/`, then merges both
# branches back to the driver branch sequentially. Aggregates the
# integration tests under `loom-workflow/tests/parallel.rs` plus the
# concurrency-overlap unit tests under `run::parallel`.
#-----------------------------------------------------------------------------
#-----------------------------------------------------------------------------
# test_git_client_roundtrip — `GitClient` exercises create-worktree, list,
# status, merge (clean / non-conflicting / conflict variants), and remove
# against a temp repo via the typed Rust API. Cargo integration tests live
# in `loom-driver/tests/git_client.rs`.
#-----------------------------------------------------------------------------
#-----------------------------------------------------------------------------
# test_state_db_lifecycle — `StateDb::open` on a fresh path creates schema;
# `rebuild` populates from `specs/*.md` plus mock `bd` output and resets
# iteration counters; `recreate` recovers from a corrupted file. Aggregates
# the lifecycle subset of `loom-driver/tests/state_db.rs`.
#-----------------------------------------------------------------------------
#-----------------------------------------------------------------------------
# test_per_spec_locking — two contending acquisitions on the same
# `<label>.lock` serialize via `flock`; the second waits via `MockClock`
# advance, then errors naming the held label. A crashed child releases the
# lock immediately so the parent re-acquires. Aggregates the integration
# tests under `loom-driver/tests/lock_manager.rs`.
#-----------------------------------------------------------------------------
#-----------------------------------------------------------------------------
# test_logging_tee_equality — renderer and on-disk `.jsonl` log subscribe
# to the same `AgentEvent` stream; capturing both yields line-for-line
# equality on the log side. Exercises the integration test under
# `loom-driver/tests/logging.rs::run_single_event_sink_property`.
#-----------------------------------------------------------------------------
#-----------------------------------------------------------------------------
# Annotation gate — bidirectional integrity check on `specs/*.md`
# annotations against `tests/loom-test.sh`. Implemented in
# `loom/crates/loom/tests/annotations.rs` per spec §Architecture /
# Annotation Integrity Gate.
#-----------------------------------------------------------------------------

#-----------------------------------------------------------------------------
# Property-based tests — `proptest` invariants under
# `loom/crates/{loom-driver,loom-agent}/tests/properties.rs`. Each shell
# function dispatches into the matching cargo integration test. CI runs
# at `PROPTEST_CASES=32` (set in tests/loom/default.nix); local exhaustive
# runs override via env var (`PROPTEST_CASES=2048+`).
#-----------------------------------------------------------------------------

# JSONL line parser invariants: never panics on arbitrary bytes, never emits
# an `AgentEvent` from a malformed line, and `MAX_LINE_BYTES` matches the
# spec's 10 MB cap. Exercises both PiParser and ClaudeParser since the
# JSONL framing contract is shared between backends.
# Pi protocol parser invariants: round-trip identity for known shapes
# (prompt/steer encoders), unknown message types surface as
# `ProtocolError::UnknownMessageType`, never panics on arbitrary bytes.
# Claude stream-json parser invariants: round-trip identity for known
# shapes (System, Result, encoded user message), unknown shapes hit the
# `Unknown` variant via `#[serde(other)]`, never panics on arbitrary bytes.
# State DB rebuild invariants: arbitrary spec content never corrupts the
# schema, `recreate` always recovers from a corrupted DB file, and
# round-trips of known molecule shapes are stable.
# Default case count of 32 is baked into each `proptest!` block via
# `ProptestConfig::with_cases(32)`. The env var still overrides upward
# for local exhaustive runs (`PROPTEST_CASES=2048 …`).
#-----------------------------------------------------------------------------
# Snapshot tests — `insta` snapshots for contract surfaces.
#
# Templates and CLI help are user-visible contracts where layout drift slips
# silently past substring assertions. Snapshots fail loudly on any text
# change; PR reviewers see the rendered diff. The run-time renderer is a
# flexibility surface and is excluded by `renderer_no_insta_dependency`.
#-----------------------------------------------------------------------------

# Every typed Askama context has at least one `.snap` under
# loom/crates/loom-templates/tests/snapshots/. Names are tied to the test
# function via insta's default `<test-binary>__<test-name>.snap` shape.
# Every `loom <subcommand> --help` surface has an insta snapshot. The list
# below mirrors the v1 command surface in `crates/loom/src/main.rs::Command`.
# Run-time renderer must NOT use `insta`. Per spec §Snapshot Testing, the
# renderer is a flexibility surface — substring + structural assertions keep
# layout decisions free to evolve. The Rust check lives at
# `loom/crates/loom/tests/style.rs::renderer_no_insta_dependency`.
#-----------------------------------------------------------------------------
# Container smoke tests (specs/loom-tests.md Functional #5, NFR #7).
#
# The smoke harness lives at tests/loom/run-tests.sh and is exposed as
# `nix run .#test-loom` via tests/loom/default.nix and modules/flake/apps.nix.
# The verifications below check (a) the harness file exists, (b) the Nix
# wiring exposes the runner only on Linux, (c) Darwin gets a clear skip
# message, and (d) Linux execution stays under the 30s wall-time budget.
#-----------------------------------------------------------------------------

#-----------------------------------------------------------------------------
# Determinism — banned wall-clock primitives in production code.
#
# Each function below runs the issue-mandated grep over `loom/crates/*/src/`
# and asserts the only matches live in `SystemClock`'s impl (and `MockClock`
# for the tokio-time primitives, which the test backend must use to
# participate in tokio's paused-time runtime).
#-----------------------------------------------------------------------------

# Allowed file paths for `tokio::time::sleep` matches.
_loom_allowed_clock_files() {
    cat <<'EOF'
loom/crates/loom-driver/src/clock/system.rs
loom/crates/loom-driver/src/clock/mock.rs
EOF
}

# Filter `path:line:body` matches to those whose path is in the allow-list.
_loom_filter_disallowed() {
    local allow
    allow=$(_loom_allowed_clock_files)
    grep -vF -f <(printf '%s\n' "$allow") || true
}

#-----------------------------------------------------------------------------
# Stubs for tests whose implementation hasn't landed yet. Each returns 77
# (skip) so the verify gate resolves the spec annotation without claiming
# coverage. As each feature lands, replace the body and drop this banner.
#-----------------------------------------------------------------------------

# Filesystem Lock Map

# Verdict Gate

# Recovery & Iteration

# Plan / Todo
#-----------------------------------------------------------------------------
# test_plan_new_writes_implementation_notes — `loom plan -n` seeds the
# implementation-notes table for the new spec via `loom note set`. Two
# unit tests pin this contract:
#   1. The rendered plan_new prompt names `loom note set <label>
#      --kind implementation` so the agent has a concrete invocation to
#      copy at the end of the interview.
#   2. The runner, when invoked with `PlanMode::New`, threads the prompt
#      through `wrapix run` such that the agent receives the seeding
#      instruction (argv[4] is the rendered prompt body).
# Together these ensure the only path that produces notes during -n
# routes through the `loom note set` CLI — `ensure_spec_row` on that
# call is what inserts the `specs` row, matching the criterion.

#-----------------------------------------------------------------------------
# test_todo_renders_notes_into_beads — `loom todo` reads implementation
# notes from the anchor's `notes` rows (kind = 'implementation') and renders
# each note's text into the prompt so the agent copies them into every new
# bead body. Productive completion atomically consumes the notes and advances
# the cursor via `StateDb::consume_notes_and_advance_cursor`.

#-----------------------------------------------------------------------------
# Bare `loom logs` against an empty `.wrapix/loom/logs/` exits 0 with a
# "No bead logs yet" message; `--path` against the same state exits non-zero.
# Runs the binary via `cargo run` so the empty-logs branch is exercised
# end-to-end (`run_logs` translates `LogsError::NoLogs` into the friendly
# message for the bare path and the typed error for `--path`).

#-----------------------------------------------------------------------------
# `loom logs -f` keeps reading past EOF until interrupted. The unit test
# under loom-workflow drives a paused-time tokio runtime that proves the
# poll loop advances Clock::sleep after EOF — the same code path the
# binary takes with a real SystemClock and no follow_max_polls cap.

#-----------------------------------------------------------------------------
# `--path` is mutually exclusive with `-f`, `-v`, `--raw` (clap-enforced).
# The pinned `cli_help` snapshot for `loom logs --help` already records the
# `Mutually exclusive` notes; this dispatcher pins runtime rejection by
# running the binary with conflicting flag pairs and asserting non-zero exit.

#-----------------------------------------------------------------------------
# `--raw` copies file bytes verbatim and `-f --raw` polls past EOF.

#-----------------------------------------------------------------------------
# Same renderer types fed by `loom run` are constructed by `loom logs`.
# Pinning both: the unit-test round-trip through JSONL exercises
# `loom_render::build_renderer`, and the workflow-level test exercises
# the same `build_renderer` selection inside `logs_cmd::replay`.

#-----------------------------------------------------------------------------
# `-v` streams `TextDelta` text verbatim during render — same widening as
# `loom run -v` (the renderer's `Verbose` mode handles both call sites).
#-----------------------------------------------------------------------------
# test_snapshots_no_crate_root_allows — the snapshot tests must inherit the
# workspace clippy exemptions in loom/clippy.toml (allow-*-in-tests = true)
# rather than re-declare a crate-root `#![allow(clippy::unwrap_used, ...)]`.
# Per specs/loom-templates.md § Snapshot Tests + workspace policy.
#-----------------------------------------------------------------------------
test_snapshots_no_crate_root_allows() {
    local f="$LOOM_DIR/crates/loom-templates/tests/snapshots.rs"
    if [ ! -f "$f" ]; then
        echo "missing snapshot test file: $f" >&2
        return 1
    fi
    if grep -nE '^#!\[allow' "$f"; then
        echo "crate-root #![allow(...)] in $f: must rely on workspace clippy.toml exemptions" >&2
        return 1
    fi
}

# Stubs added by the spec-authoring-conventions planning session —
# pending implementation of: --tree handoff, outer-loop iteration,
# tree-review push-gate blocking, bare-loom grouped help, gate
# decision function production wiring, consolidated pinning matrix
# verifications, spec_conventions partial + LoomConfig field,
# rule-family-agnostic partials.
#-----------------------------------------------------------------------------
# test_run_execs_check_then_review_tree — FR1 molecule-completion handoff:
# `loom run`'s outer loop invokes `loom gate verify --tree -s <label>` first,
# then `loom gate review --tree -s <label>`, both unconditionally. The
# verify-then-review ordering and the `--tree` scope on both invocations
# are asserted by a recording stub script in the production test.

#-----------------------------------------------------------------------------
# test_run_outer_loop_iterates_on_fixups — FR1 outer loop: after the
# molecule-completion handoff returns, `run_loop` re-polls `bd ready`,
# processes any newly-ready fix-up beads, and only exits when (a) no new
# fix-ups appear after a handoff (stall), or (b) the `[loop]
# max_iterations` counter is exhausted.
#-----------------------------------------------------------------------------
#-----------------------------------------------------------------------------
# test_style_rules_pinning_matrix — run.md and review.md include
# partial/style_rules.md; the other phase templates (plan_new, plan_update,
# todo_new, todo_update, msg) do NOT include it. Matches the pinning matrix
# in specs/loom-templates.md.
#-----------------------------------------------------------------------------
test_style_rules_pinning_matrix() {
    local templates_dir="$LOOM_DIR/crates/loom-templates/templates"
    local include_re='\{%[[:space:]]+include[[:space:]]+"partial/style_rules\.md"[[:space:]]*%\}'
    local missing=0 t f
    for t in run review; do
        f="$templates_dir/$t.md"
        if [ ! -f "$f" ]; then
            echo "missing template: $f" >&2
            missing=$((missing + 1))
            continue
        fi
        if ! grep -qE "$include_re" "$f"; then
            echo "template $t.md must include partial/style_rules.md" >&2
            missing=$((missing + 1))
        fi
    done
    for t in plan_new plan_update todo_new todo_update msg; do
        f="$templates_dir/$t.md"
        if [ -f "$f" ] && grep -qE "$include_re" "$f"; then
            echo "template $t.md must NOT include partial/style_rules.md" >&2
            missing=$((missing + 1))
        fi
    done
    return "$missing"
}
#-----------------------------------------------------------------------------
# test_spec_conventions_pinning_matrix — plan_new.md and plan_update.md
# include partial/spec_conventions.md; the other phase templates (todo_new,
# todo_update, run, review, msg) do NOT include it. Matches the pinning
# matrix in specs/loom-templates.md.
#-----------------------------------------------------------------------------
test_spec_conventions_pinning_matrix() {
    local templates_dir="$LOOM_DIR/crates/loom-templates/templates"
    local include_re='\{%[[:space:]]+include[[:space:]]+"partial/spec_conventions\.md"[[:space:]]*%\}'
    local partial="$templates_dir/partial/spec_conventions.md"
    local missing=0 t f
    if [ ! -f "$partial" ]; then
        echo "missing partial: $partial" >&2
        missing=$((missing + 1))
    elif ! grep -qE '\{\{[[:space:]]*spec_conventions[[:space:]]*\}\}' "$partial"; then
        echo "partial does not render {{ spec_conventions }}: $partial" >&2
        missing=$((missing + 1))
    fi
    for t in plan_new plan_update; do
        f="$templates_dir/$t.md"
        if [ ! -f "$f" ]; then
            echo "missing template: $f" >&2
            missing=$((missing + 1))
            continue
        fi
        if ! grep -qE "$include_re" "$f"; then
            echo "template $t.md must include partial/spec_conventions.md" >&2
            missing=$((missing + 1))
        fi
    done
    for t in todo_new todo_update run review msg; do
        f="$templates_dir/$t.md"
        if [ -f "$f" ] && grep -qE "$include_re" "$f"; then
            echo "template $t.md must NOT include partial/spec_conventions.md" >&2
            missing=$((missing + 1))
        fi
    done
    return "$missing"
}

# Compaction Recovery (scratch dir). Kept at the tail of the test
# functions so the loom-doctor body-slicing heuristic — which scans
# from each `test_*(` header to the next `test_*(` for `_pending_stub`
# substrings — does not pick up the `_pending_stub()` helper definition
# above and flag these promoted-to-real dispatchers as still stubbed.
scratch_cargo_test() {
    cargo_run test -p loom-driver --lib "scratch::tests::$1" -- --exact --nocapture --quiet
}

# Pinning-matrix audit. Body is a single-line invocation so the body-slicing
# heuristic cannot mistake stray text below for a stub call.
test_pinning_matrix_audit() { cargo_run run --quiet --bin loom -- --workspace "$REPO_ROOT" check matrix; }

#-----------------------------------------------------------------------------
# test_check_surface_detects_drift — FR13 surface audit. The dispatcher pins
# both the live binary path (`loom check surface` exits 0 on the current
# code-spec pair) AND the per-drift unit tests under
# `loom-workflow::check::surface`. The unit tests cover each FR13 dimension
# (command set, flag set, removed surface, grouping order); the live binary
# run is the production-path smoke test that catches drift in the wiring or
# spec markdown.

#-----------------------------------------------------------------------------
# Dispatch
#-----------------------------------------------------------------------------
if [ $# -eq 0 ]; then
    echo "usage: $0 <test_function>" >&2
    echo "available functions:" >&2
    declare -F | awk '{print "  " $3}' | grep '^  test_' >&2
    exit 2
fi

fn="$1"
shift
if ! declare -F "$fn" >/dev/null; then
    echo "unknown function: $fn" >&2
    exit 2
fi
"$fn" "$@"
