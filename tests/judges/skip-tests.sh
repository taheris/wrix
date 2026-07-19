#!/usr/bin/env bash
set -euo pipefail

# Judge rubrics for skip-tests success criteria

test_binary_guards_removed() {
  judge_files "tests/mcp/tmux/test_lib.sh" "tests/mcp/tmux/e2e/test_sandbox_debug_profile.sh" "tests/mcp/tmux/e2e/test_mcp_in_sandbox.sh" "tests/mcp/tmux/e2e/test_mcp_audit_config.sh" "tests/mcp/tmux/e2e/test_filesystem_isolation.sh" "tests/mcp/tmux/e2e/test_profile_composition.sh"
  judge_criterion "Binary availability guards (command -v / which checks that skip or exit early when binaries like tmux-mcp, nix, or podman are missing) have been removed from tests where those binaries are provided by the runner environment"
}
