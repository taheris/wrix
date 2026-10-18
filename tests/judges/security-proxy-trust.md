#!/usr/bin/env bash

set -euo pipefail

: <<'RUBRIC'

# Security, cache and proxy trust review

These are documentation rubrics, not evidence of working runtime isolation,
credential delivery or network enforcement. Evaluate the selected section
against its declared files using semantic review, not keyword matching. PASS
requires all listed distinctions, with no contradictory stronger guarantee
elsewhere in the declared documentation. Report missing or misleading claims
with file and section evidence. Existing provider, Git, Pi, execution-evidence
and default-off diagnostics verifiers remain independent; these rubrics do not
replace them.

## Shared Service Boundary

```bash
RUBRIC
shared-service-boundary() {
  judge_files "specs/security.md" "specs/cache.md" "specs/services.md" \
    "specs/profiles.md" "specs/proxy.md" "docs/architecture.md"
  judge_criterion "PASS only if the documentation makes every distinction below:
1. Workspace namespaces and ordinary client API authorization are logical access controls, not separate compromise/fault boundaries. Dolt, Celler, proxy and the actual shared sccache daemon are trusted cohosted infrastructure, not agent sandboxes or a separately isolated compiler service.
2. Delegated worker compiler jobs, including procedural macros, intentionally execute inside that service trust boundary. They can affect service-visible Dolt/private data, proxy policy/state, Celler metadata/archives and delegated Nix cache-signing authority, and thereby host-cache integrity, without worker sandbox escape, key theft or a direct worker Nix-upload feature. This is neither compiler-only isolation nor a universally stronger isolation claim than personal-host cache sharing.
3. Celler signing state is service-visible delegated authority. Host-private JWT issuer keys, publishing/management credential files and authoritative manifests remain outside service mounts, as do general host filesystem and host Nix store/daemon authority. Unmounted files do not imply confidentiality of scoped runtime authorization presented in legitimate service requests: a compromised service can observe it. Ordinary traffic/pull credentials confer only their declared scope, not signing, publishing, issuer or host-management authority. Celler Nix cache signing is distinct from explicitly granted agent Git commit-signing keys and host trusted-Nix configuration; neither Git grants nor repository identity enlarge infrastructure authority.
4. Service-side compiler CPU/memory use belongs to the service budget, not automatically the worker's limits. Restricted Rust must select private/local compilation and receives no shared compiler execution route, so its own compiler code cannot bypass worker network restrictions through delegation. Proxy enforcement still assumes trusted service infrastructure, including other cohosted jobs. Private tool mode does not protect Celler-private data or signing/backend state from privileged service code.
5. Full agent/Cargo runtimes retain existing worker boundaries. No shared parent Nix daemon/store, physical-host store/daemon mount, containerd/nix-snapshotter migration, full worker-runtime migration or new compiler-only isolation is implied.
6. The shared-service discussion preserves existing evidence limits: default-off, operator-enabled component diagnostics are separate and non-authoritative, not automatically indexed or aggregated as audit content; components own their diagnostic contracts and operators own destination/access/retention/deletion. Execution, conversation and focus identities remain distinct; incomplete completion is unknown, transcripts/codemode are non-exhaustive evidence, and missing evidence does not prove no action. No new collector, credential broker, automatic recovery or adversarial-agent detection is promised."
}
: <<'RUBRIC'
```

## Cache Sharing Authority

```bash
RUBRIC
cache-sharing-authority() {
  judge_files "specs/security.md" "specs/cache.md" "specs/profiles.md" \
    "specs/services.md" "specs/proxy.md" "docs/architecture.md"
  judge_criterion "PASS only if the documentation makes every distinction below:
1. Ordinary Celler read access is the authenticated approved-public common view plus the client's own canonical repository-private view, with exact endpoints, keys and pull grants. Loom clones share canonical repository identity; unrelated private views do not merge. Physical deduplication, knowledge of private hashes, supplied cache names, mutable repository policy and agent requests do not grant membership, approve public sources or broaden publication audiences. Common population requires verified identity against independently host-approved public sources, not package provenance, build success or ambient substituters. This is not a sharing-group service or unauthenticated public cache.
2. A valid input-addressed cache signature authenticates accepted metadata/content, not correct derivation execution or trustworthy build-time check results. Host trust excludes direct sandbox-upload views/keys even within the same repository: server signing or matching repository identity does not promote worker outputs into host-trusted common/private publication or peer trust. Workers remain pull-only; guest-only Nix outputs are not harvested. Independent trusted host builds remain eligible. These API/configuration rules are not protection from accepted cohosted compiler execution affecting signing/backend state and host-cache integrity.
3. Native signed bearer pull tokens have nominal one-day sandbox and seven-day host lifetimes and exact named pull-only grants, including authenticated common reads. Current host policy controls new issuance/renewal, not already-valid tokens. After authorization withdrawal an existing valid bearer can read newly published contents until native expiry, from any client able to reach the endpoint; repository labels and Basic Auth usernames do not bind it. No exact wall-clock rejection guarantee, per-request current-policy lookup, denylist, policy-triggered active-download cancellation, immediate revocation or recall of imported data is claimed. JWT verification-key rotation is distinct from Nix signing-key rotation.
4. Host activation, including unchanged/cached direnv reload, refreshes host-private credentials from current policy; managed host build setup uses that path. There is no host background renewer or interception of every native Nix command, and an unreloaded host context can expire. The host token has a longer residual exposure window and is not copied into sandboxes; sandbox credentials are separate and launcher-renewed. Do not apply proxy connection-owned revocation semantics to cache bearer expiry or imply token expiry deletes installed paths.
5. Built-in Cargo registry/git, sccache and uv data are sandbox-only: no personal-host bind, prewarm copy, write-back or fallback. Shared mode intentionally accepts one cross-repository worker cached-data/artifact trust domain, including launches with different credential grants; private sources/packages/compiled contents may be read or influenced by peers. Checksums/locks are not repository authorization or correct-build attestation. Dedicated runtime credential/configuration files and management authority are not pooled.
6. Sharing off is coarse, all-applicable-cache, fresh private writable storage for that launch with local compilation, no shared pool reads/writes/prewarm or host fallback. It cannot recall prior shared bytes, sanitize arbitrary workspace/explicit mounts, or provide selective-source privacy or private-code tracking into compiled artifacts. Private tool mode is not protection against compromised service-side code. Tool sharing does not broaden Celler views, enable worker Nix uploads, share raw Cargo targets or persist guest Nix stores/databases.
7. Repository tool-sharing policy is an editable default, unlike host-controlled Celler audience authority, not an immutable host privacy ceiling. Explicit invocation selection wins in both directions without policy writes or another environment policy channel. Workspace writers can change future inherited defaults. The --no-cache tool selection does not disable Nix/Celler integration."
}
: <<'RUBRIC'
```

## Proxy Enforcement Limits

```bash
RUBRIC
proxy-enforcement-limits() {
  judge_files "specs/security.md" "specs/proxy.md" "specs/profiles.md" \
    "specs/sandbox.md" "docs/architecture.md"
  judge_criterion "PASS only if the documentation makes every distinction below:
1. The allowlist rubric accepts residual policy-leakage risk: each base or profile-added entry is paired with a specific credential whose autonomy/exfiltration risk is accepted, or is credentialless with risk bounded by what anonymous requests can leak. Profiles owns membership and host/port mapping; security owns this rubric, not a duplicate allowlist.
2. Authenticated per-launch HTTP/CONNECT admission checks requested authorities/ports and actual resolved connection destinations under the protected address baseline; independent grants are not unioned. Traffic credentials do not grant host management. This enforcement remains useful but assumes trusted proxy/shared-service infrastructure, not compromise isolation from cohosted compiler jobs.
3. CONNECT is opaque transport without TLS termination, substituted certificates, CA injection or payload inspection. Requested authority admission is not enforcement of encrypted SNI/HTTP application identity, URL paths, repository identity or content. Shared-IP virtual hosting and exfiltration through permitted services remain possible; allowlisting is not exfiltration-proof, even with authentication and firewall isolation. No stronger claim elsewhere in the reviewed documentation negates these limits.
4. Proxy connection-owned grant loss/revocation is separate from cache native bearer expiry and from agent supervision/audit completion. It does not prove the agent stopped or repair an incomplete execution record. No TLS inspector, provider credential broker, diagnostic collector, runtime migration or expanded authoritative audit surface is introduced to claim these guarantees."
}
: <<'RUBRIC'
```

RUBRIC
