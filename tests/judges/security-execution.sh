#!/usr/bin/env bash
set -euo pipefail

test_execution_and_transcript_evidence_limits() {
  judge_files "docs/architecture.md" "docs/README.md" "specs/security.md" \
    "crates/wrix-sandbox/src/command/launch.rs" \
    "crates/wrix-sandbox/src/command/launch/execution_metadata.rs" \
    "crates/wrix-sandbox/tests/execution_metadata.rs" \
    "lib/sandbox/linux/entrypoint.sh" "lib/sandbox/darwin/entrypoint.sh" \
    "tests/security/execution-metadata-lifecycle.sh" \
    "tests/security/test_execution_lifecycle.py" "tests/security/execution-runtime.sh" \
    "tests/security/execution-probe.sh" "tests/sandbox/fixtures/command-runner.nix"
  judge_criterion "PASS only if documentation and implementation distinguish execution identity, optional agent conversation identity, and opaque notification focus target. One host-owned, secret-free index record precedes services/container work; observed completion replaces the original record, including failed attempts. Incomplete means unknown completion, not evidence of running/stopped work. Status is the foreground runtime-command wait result, not independently observed agent status; numeric 137 is not inferred to be a signal. Optional conversation identity must not come from the latest shared transcript, and known session roots use container coordinates with /workspace mapped to the selected host workspace. Transcripts and codemode summaries are non-exhaustive evidence: missing content does not prove absence of an action, intermediate tool results and effects need not be recorded, and direct runners without transcripts do not receive synthesized conversation content. No automatic recovery, lifecycle reconciliation daemon, global/nested-tool audit aggregator, adversarial-agent detection, or power-loss guarantee is promised. The live verifier must launch the actual packaged Rust host launcher and real platform containers, explicitly skip unavailable prerequisites rather than simulate a passing launch, observe initial records before normal exits/startup failures, kill the actual host launcher after guest readiness, and independently bound and clean up containers. Controlled fixture behavior has conformance coverage, while completion-write failure semantics remain proven by native Rust tests rather than a shell replica of the journal. Explicit default-off component diagnostics stay separate and non-authoritative: components own their contracts and enabling operators own destinations, access control, retention, and deletion."
}
