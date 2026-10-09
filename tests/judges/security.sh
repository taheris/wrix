#!/usr/bin/env bash
set -euo pipefail

test_agent_transcript_audit_fit() {
  judge_files "docs/architecture.md" "specs/security.md" "lib/sandbox/linux/entrypoint.sh" "lib/sandbox/darwin/entrypoint.sh"
  judge_criterion "Runtime-provided agent transcripts are fit-for-purpose audit content for the stated policy-leakage threat model without overstating direct mode. PASS only if the threat model is explicitly limited to a misbehaving but non-adversarial agent; Claude and Pi transcript locations preserve intent/reasoning and outcomes needed for post-hoc review; external direct runners own any transcript they persist; Wrix's placeholder direct runner is not claimed to synthesize transcript content; and the metadata index makes sessions and available transcripts findable."
}

test_scoped_component_diagnostics_policy() {
  judge_files "specs/security.md" "docs/architecture.md"
  judge_criterion "Component diagnostics supplement debugging, not authoritative Wrix execution or transcript evidence. PASS only if they are explicit and default-off; Wrix does not automatically enable, index, synthesize, or aggregate them into its security audit surface; each component owns its configuration, format, emission, and disclosure of secret-bearing content; and the enabling operator owns the destination, access control, retention, and deletion. Do not require any particular component to provide diagnostics. Execution metadata and transcript limitations are evaluated separately."
}
