#!/usr/bin/env bash
set -euo pipefail

test_native_workflow_guidance() {
  judge_files "docs/tmux.md" "README.md" "docs/architecture.md" "lib/sandbox/image.nix" "tests/tmux/workflow.sh" "specs/tmux.md"
  judge_criterion "PASS only if concise agent-accessible guidance ships inside the sandbox and describes direct upstream tmux commands: a workflow-private socket, create/list and native pane identifiers reused across shell calls, a detached dev server and bounded request/capture, literal text (-l) separately from special keys, remain-on-exit set BEFORE starting a fast workload, running versus dead pane status and exit-code inspection, and cleanup of only the caller's session. Distinguish finite scrollback from durable caller-managed application logs and caller-owned cleanup from container shutdown. The live packaged-sandbox verifiers must exercise those commands, including shutdown isolation. Do not promise automatic conversation cleanup, a replacement process wrapper, cross-container debugging, persistence, or recovery."
}
