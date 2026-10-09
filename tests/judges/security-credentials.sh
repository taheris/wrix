#!/usr/bin/env bash
set -euo pipefail

test_repository_git_policy_trust_boundary() {
  judge_files "README.md" "docs/architecture.md" "specs/security.md" "specs/cli.md" "specs/sandbox.md"
  judge_criterion "Security documentation consistently describes repository wrix.toml as trusted, mutable launch input, not agent-resistant authorization storage. PASS only if workspace writers can change inherited Git grants on subsequent launches, explicit caller overrides still take precedence, deploy/sign grants independently govern only Wrix-managed key delivery, and absent keys imply neither a read-only workspace nor no network access. A deploy grant conveys the key's actual repository permissions, not a constrained publishing transaction. Container signing overrides host signing policy only for that execution without rewriting common repository Git config. Do not claim an independent policy store, credential broker, or protection from deliberately caller-supplied credentials; preserve the separate service identity and provider-auth storage/locking contracts."
}
