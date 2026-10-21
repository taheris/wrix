#!/usr/bin/env bash set -euo pipefail

: <<'RUBRIC'

# Native cache authorization lifecycle

This rubric reviews the documented model, not deployed credential delivery or
platform acceptance. Pending worker, renewal, routing and expiry tests remain
missing evidence; component fixtures cannot satisfy them by implication.

## Native Expiry

```bash
RUBRIC
native-expiry() {
  judge_files "specs/cache.md" "specs/security.md" "specs/services.md" \
    "docs/architecture.md"
  judge_criterion "PASS only if the documentation consistently distinguishes all of the following:
1. Sandbox read credentials are native signed bearer tokens with a nominal one-day lifetime and exact authenticated common and canonical-private pull grants. Pull authority does not imply management, publishing, signing or issuer authority. The issuer private key stays host-side and is distinct from backend Nix cache-signing keys; there is no host pull-token lifecycle.
2. Current host-controlled authorization determines new issuance and renewal. Repository input, stale grants, Basic Auth usernames and caller-supplied cache names cannot enlarge that authorization. Withdrawal denies subsequent issuance for the withdrawn audience but does not revoke an already-valid bearer, including its access to contents published after withdrawal.
3. Bearers can be reused by another client able to reach the endpoint. Native JWT verification and expiry are not per-request current-membership checks, denylists, exact wall-clock rejection guarantees, policy-triggered active-download cancellation or recall of previously imported bytes. JWT verification-key rotation and Nix signing-key rotation have different effects and are not interchangeable revocation mechanisms.
4. Waiting-launcher renewal is distinct from controller issuance and proxy connection-owned grants. A persistent controller does not imply detached renewal, agent supervision, automatic session recovery or continued credential refresh after launcher exit. Renewal failure neither kills the agent nor widens rights or extends an old token's expiry. Suspension and clock changes can interrupt authentication; uninterrupted renewal is not promised.
5. Native authorization/component fixtures are distinguished from actual credential delivery, directory-mount reload, deployed worker admission and Podman/krun/Apple acceptance. Host publication-only exclusion remains intact; a valid shared-cache signature is not correct-build evidence or authority for host substitution. Missing or skipped platform evidence is not another platform's pass."
}
: <<'RUBRIC'
```

RUBRIC
