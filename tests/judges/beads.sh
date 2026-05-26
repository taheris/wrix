#!/usr/bin/env bash
# Judge rubrics for beads.md success criteria

test_sync_in_container() {
  judge_files "lib/sandbox/linux/entrypoint.sh" "lib/sandbox/darwin/entrypoint.sh"
  judge_criterion "In the container environment, bd is wired to the host's shared dolt instance via BEADS_DOLT_SERVER_SOCKET (bound to /workspace/.wrapix/dolt.sock) with BEADS_DOLT_AUTO_START disabled. Through this socket, bd dolt pull and bd dolt push operate against the host-shared dolt database (remote-to-host sync is a separate, host-side concern). A missing socket is fatal rather than silently falling back to a per-container dolt."
}

test_shellhook_fail_loud() {
  judge_files "lib/beads/default.nix"
  judge_criterion "When a beads workspace is detected (\$PWD/.beads/dolt exists), beads.shellHook fails non-zero with a stderr message — and never falls back to an embedded dolt — in both fail-loud branches: (a) the missing-runtime branch, where Darwin requires either 'container' or 'podman' on PATH and Linux requires 'podman' on PATH, and (b) the unreachable-dolt branch in waitAndExport, where dolt must become reachable within the bounded startup budget (Darwin: TCP 127.0.0.1:\$port; Linux: a Unix socket at the beads-dolt-reported path) before the loop gives up. Both branches must emit a stderr diagnostic identifying the failure cause and terminate the shellHook with non-zero status (return 1 / exit 1) instead of leaving BEADS_DOLT_AUTO_START enabled or otherwise permitting bd to silently spawn an embedded dolt."
}

test_workspace_naming_determinism() {
  judge_files "lib/beads/default.nix"
  judge_criterion "The _hash, _name, and _port helpers derive the per-workspace container name and port deterministically from the workspace path so that (a) the same workspace path always yields the same name and the same port across invocations — _hash is a pure function of the path (sha256sum of the path, truncated to 8 hex chars), _name is basename(path)-beads, and _port is 13306 + (hash mod 500), each computed from \$1 (falling back to \$PWD) with no hidden time- or env-dependent inputs — and (b) collision risk between distinct workspace paths is bounded by the sha256 derivation: ports live in a fixed [13306, 13805] window of 500 slots, and the hash-to-port mapping uses sha256 truncation rather than a path-component shortcut, so name collisions require basename collisions and port collisions require sha256-prefix collisions over the 500-slot window rather than being structurally forced by path similarity."
}
