#!/usr/bin/env bash
# Judge rubrics for beads.md success criteria

test_sync_in_container() {
  judge_files "lib/sandbox/linux/entrypoint.sh" "lib/sandbox/darwin/entrypoint.sh"
  judge_criterion "In the container environment, bd is wired to the host's shared dolt instance via BEADS_DOLT_SERVER_SOCKET (bound to /workspace/.wrapix/dolt.sock) with BEADS_DOLT_AUTO_START disabled. Through this socket, bd dolt pull and bd dolt push operate against the host-shared dolt database (remote-to-host sync is a separate, host-side concern). A missing socket is fatal rather than silently falling back to a per-container dolt."
}

test_shellhook_lifecycle_isolation() {
  judge_files "lib/beads/default.nix"
  judge_criterion "On Linux, beads.shellHook starts the dolt container with a lifecycle independent of the caller process's cgroup. PASS if the Linux branch probes for user-systemd availability (e.g. command -v systemd-run combined with systemctl --user is-active dbus.service) and, when available, wraps the beads-dolt start invocation in systemd-run --user --scope (with --collect for transient-scope cleanup), so the container's conmon and descendants land in a transient user scope rather than inheriting the caller's cgroup. When user systemd is unavailable (sandbox container, non-systemd host), the shellHook must fall back to the direct beads-dolt start invocation so those environments continue to work. Both code paths must propagate non-zero exit status from beads-dolt start as a shellHook failure (no fallback to embedded dolt). The Darwin branch needs no equivalent — Apple's container and podman-via-VM already isolate via separate process trees."
}

test_shellhook_fail_loud() {
  judge_files "lib/beads/default.nix"
  judge_criterion "When a beads workspace is detected (\$PWD/.beads/dolt exists), beads.shellHook fails non-zero with a stderr message — and never falls back to an embedded dolt — in both fail-loud branches: (a) the missing-runtime branch, where Darwin requires either 'container' or 'podman' on PATH and Linux requires 'podman' on PATH, and (b) the unreachable-dolt branch in waitAndExport, where dolt must become reachable within the bounded startup budget (Darwin: TCP 127.0.0.1:\$port; Linux: a Unix socket at the beads-dolt-reported path) before the loop gives up. Both branches must emit a stderr diagnostic identifying the failure cause and terminate the shellHook with non-zero status (return 1 / exit 1) instead of leaving BEADS_DOLT_AUTO_START enabled or otherwise permitting bd to silently spawn an embedded dolt."
}

test_workspace_naming_determinism() {
  judge_files "lib/beads/default.nix"
  judge_criterion "The _hash, _name, and _port helpers derive the per-workspace container name and port deterministically from the workspace path so that (a) the same workspace path always yields the same name and the same port across invocations — _hash is a pure function of the path (sha256sum of the path, truncated to 8 hex chars), _name is basename(path)-beads, and _port is 13306 + (hash mod 500), each computed from \$1 (falling back to \$PWD) with no hidden time- or env-dependent inputs — and (b) collision risk between distinct workspace paths is bounded by the sha256 derivation: ports live in a fixed [13306, 13805] window of 500 slots, and the hash-to-port mapping uses sha256 truncation rather than a path-component shortcut, so name collisions require basename collisions and port collisions require sha256-prefix collisions over the 500-slot window rather than being structurally forced by path similarity."
}

test_beadspush_pushes_before_pulls() {
  judge_files "scripts/beads-push"
  judge_criterion "On the happy path, beads-push invokes 'bd dolt push' before any 'bd dolt pull'. PASS if the dolt-sync sequence runs 'bd dolt commit' (best-effort) followed by 'bd dolt push' as the first remote-mutating call, and the 'bd dolt pull' invocation lives only inside a fallback branch that is entered when (and only when) the initial push exited non-zero. The fallback must itself only be triggered for fast-forward / non-fast-forward style rejections (e.g. the script inspects captured push stderr for 'non-fast-forward', 'reject', 'behind', 'out of date', or equivalent patterns); other push failures (network, auth, disk) must propagate the original push exit code without ever calling 'bd dolt pull'. The net effect against an up-to-date remote is that 'bd dolt pull' is never executed, so the session-close run never enters the Dolt merge path."
}

test_beadspush_failloud_on_intent_overwrite() {
  judge_files "scripts/beads-push"
  judge_criterion "On the pull-fallback path, beads-push snapshots local status and labels intent BEFORE running 'bd dolt pull', re-reads the same rows after the pull, and refuses to push when post-pull state diverges from the snapshot. PASS if all of the following hold: (a) the affected-issue set is derived from a Dolt system-table column diff between local HEAD and the remote-tracking branch (queries against dolt_commit_diff_issues and dolt_commit_diff_labels, or the equivalent dolt_diff_* per-table system tables, filtered to rows where status changed or labels were added/removed); (b) the pre-pull snapshot captures both status and the labels set for each affected issue (e.g. via SELECT against issues joined to a GROUP_CONCAT over labels, or equivalent); (c) the same snapshot query runs again after 'bd dolt pull' and the two outputs are compared (diff, equality check, or row-by-row comparison); (d) on any divergence, the script writes the affected issue IDs to stderr and exits non-zero WITHOUT calling 'bd dolt push' a second time; (e) the divergence check covers updates only — insert/delete row collisions are out of scope and need not be handled."
}
