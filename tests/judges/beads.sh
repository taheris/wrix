#!/usr/bin/env bash
# Judge rubrics for beads.md success criteria

test_sync_in_container() {
  judge_files "lib/sandbox/linux/entrypoint.sh" "lib/sandbox/darwin/entrypoint.sh"
  judge_criterion "In the container environment, bd is wired to the host's shared dolt instance via BEADS_DOLT_SERVER_SOCKET (bound to /workspace/.wrapix/dolt.sock) with BEADS_DOLT_AUTO_START disabled. Through this socket, bd dolt pull and bd dolt push operate against the host-shared dolt database (remote-to-host sync is a separate, host-side concern). A missing socket is fatal rather than silently falling back to a per-container dolt."
}
