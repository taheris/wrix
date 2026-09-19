# Security

Cross-cutting security invariants for wrix sandboxes: threat model,
credential surfaces, network exfil baseline, audit anchor, and scoped component
diagnostics.

## Problem Statement

AI coding agents running inside wrix sandboxes hold three kinds of
credentials (deploy key, signing key, agent API credentials), reach network
destinations governed by wrix's `open` / `limit` egress policy, and produce
session artefacts the operator can later inspect. The per-component specs (`sandbox.md`, `image-builder.md`,
`linux-builder.md`, `profiles.md`) own the *mechanics* of each surface;
this spec owns the cross-cutting *trust model* that ties them together —
what the threat is, why each mitigation is fit for that threat, and
which invariants must hold across components. Component-specific facts
are cross-referenced, not duplicated.

## Architecture

### Threat Model

The primary realistic threat is **policy leakage**: a prompt-injected or
misbehaving agent uses its granted permissions (deploy key, network,
OAuth token, workspace access) for unintended purposes — exfiltrating
code, secrets, or credentials through legitimate channels. No kernel
escape or boundary exploit is required.

The secondary threat is **boundary escape**: the agent breaks out of
the sandbox into the host. Boundary classes and their operator controls are
owned by `sandbox.md`; this spec relies on that isolation contract.

The normal boundary model excludes host container-runtime control. The unsafe host-Podman exception defined by `sandbox.md` can start containers with host bind mounts and is therefore outside the normal sandbox mitigations.

Defense against policy leakage requires restricting *what's available
inside the boundary* (network, credentials, filesystem), not
strengthening the boundary itself. The credential and network-baseline
sections below are scoped to that threat.

The audit-anchor invariant further assumes an agent that may be
*misbehaving but not adversarial* — one that does not deliberately hide
its actions from the transcript. An adversarial-agent model (agent
attempts to evade its own logging) is a different problem class and out
of scope.

### Credential Surfaces

The credentials crossing the host-container boundary are the deploy key,
the signing key, and the agent's API credentials (claude's OAuth token, or
a non-claude agent's provider API key / auth file).

#### Deploy & Signing Keys

Both keys are ed25519, generated **passphraseless** so the agent can use
them non-interactively. Acceptable because:

- The host-side directory holding them (`~/.ssh/deploy_keys/`) is mode
  `700`; the keys themselves are `600`.
- The deploy key is **repository-scoped** — GitHub deploy keys grant
  write only to the single repository they were added to. Compromise
  blast radius is one repo, not the user's full GitHub identity.
- The signing key adds attribution to commits; it does not grant any
  additional access.

**Host-source resolution precedence.** When staging a key for the
sandbox, the launcher resolves the *host* source path. The rule below
is stated for the deploy key; the signing key follows the same rule
with `WRIX_SIGNING_KEY` and `$HOME/.ssh/deploy_keys/<name>-signing`
substituted in. The two keys are resolved independently — neither
affects the other.

1. If `WRIX_DEPLOY_KEY` is set in the launcher's environment and
   points at an existing file, that path is the source.
2. Else if `$HOME/.ssh/deploy_keys/<name>` exists, that path is the
   source.
3. Else the deploy key is not mounted.

If `WRIX_DEPLOY_KEY` is set but the pointed-at file does not exist,
the launcher **fails loudly** (non-zero exit before the container
starts, with a stderr message naming the missing path) rather than
silently falling through to the `$HOME` path. A set-but-missing env
var indicates a parent-process mistake the operator wants to see,
not a recoverable condition. Same for `WRIX_SIGNING_KEY`.

**Spawn mode requires both keys.** The rule-3 no-mount fall-through
(silent keyless boot) applies only to interactive `wrix run`. Under
`wrix spawn` — the non-interactive path loom uses for loop agents — an
unresolved key (no env pointer *and* no `$HOME/.ssh/deploy_keys/`
fallback) is fail-loud: the launcher exits non-zero before the container
starts, naming the unresolved key. A loop agent that boots keyless cannot
sign or push and only discovers the gap at land-the-plane time, after its
work is done and lost when the container exits; failing at launch turns a
wasted agent run into an immediate, actionable error. The deploy key is
always required under spawn; the signing key is required unless
`WRIX_GIT_SIGN=0` disables commit signing, in which case an unresolved
signing key is not fail-loud (a keyless boot still needs the deploy key to
push).

This precedence exists to support **nested sandboxes**: a parent
wrix container can spawn a child wrix container, injecting keys at
arbitrary host paths and passing those paths through `WRIX_DEPLOY_KEY` /
`WRIX_SIGNING_KEY`. Without this rule the child would boot without keys (the
parent's `$HOME` has no
`~/.ssh/deploy_keys/`), agents would produce unsigned commits, and
`git push` would fail.

**In-container delivery is fixed.** `sandbox.md` owns deploy-key-name
validation, mount destinations, and the child environment values. The security
invariant is that the selected host source path never crosses the boundary and
every launch applies that same delivery contract, which makes nested wrix
launches recursively composable.

**Host and repository Git bootstrap.** `cli.md` owns the `wrix init`
command that applies repo-local Git config for host shells, devshells,
containers, and Loom clones when invoked in each repository. The security
invariant is that Wrix-managed Git transport uses only the context-resolved
deploy key:
`WRIX_DEPLOY_KEY` when the launcher supplied an in-container path, or
`$HOME/.ssh/deploy_keys/<name>` on the host. If neither exists, the
operation fails instead of trying the user's default SSH identities,
SSH agent keys, or `~/.ssh/config` identities.

All Wrix-managed GitHub SSH uses strict, noninteractive host-key
verification with Wrix-pinned GitHub host keys. `StrictHostKeyChecking=no`,
runtime `ssh-keyscan` trust-on-first-use, and appending learned GitHub keys
to the user's `~/.ssh/known_hosts` are outside the security model. When Wrix
creates SSH directories or compatibility config/known-hosts files, directory
modes are `0700` and `config` / `known_hosts` file modes are `0600`.

Commit signing follows the same context-resolution rule through the signing
key (`WRIX_SIGNING_KEY` or `$HOME/.ssh/deploy_keys/<name>-signing`). Signing
is default-on for initialized repositories; missing signing material is a
hard failure unless the operator explicitly disables signing.

**Trust model.** The launcher's parent process is trusted to choose
the host source path. The launcher's only validation is presence
(`[ -f ]`); it performs no path-prefix, ownership, mode, or content
check. The justification is symmetry with the existing surface: a
hostile parent could already write to `$HOME/.ssh/deploy_keys/`, so
accepting env pointers adds no new attack surface.

#### Agent API Credentials

The agent's API credentials reach the container at **runtime only**. This
constrains the delivery path, not credential lifetime or revocation. Image
layers are content-addressed and widely shared, so secret material in a layer
leaks beyond the session that supplied it.

The environment surface is split into static defaults and runtime secrets:

- `profile.env`, `profile.hostEnv`, `mkSandbox.env`, and agent-settings env are
  non-secret static defaults. They enter Nix evaluation and may be serialized
  into a Nix-store `ProfileConfig`, OCI config `Env`, or agent-settings image
  layer. Nix rejects malformed environment names, known provider credential
  names, and every declared runtime-secret name from these static surfaces.
- `runtimeSecrets` is a typed attrset from validated environment-variable name
  to `"optional"` or `"required"`. `profiles.md` owns the built-in declaration
  names and policies. `ProfileConfig.security.runtime_secrets` contains only
  names and policies, never their values.
- Immediately before launch, the host launcher resolves each declaration. A
  matching `SpawnConfig.env` pair wins for `wrix spawn`; otherwise the launcher
  reads the same-named host environment variable. An absent optional source is
  omitted, while an absent required source fails before container startup.
  Dry-run output redacts known and declared secret values.

The second delivery channel is **isolated persistent credential storage**. For
Pi, `WRIX_PI_AUTH_FILE` selects the host auth file, otherwise
`~/.pi/agent/auth.json` is used. Interactive `wrix run` initializes an empty
missing fallback for first `/login`; a missing explicit override or missing
non-interactive `wrix spawn` credential fails before container startup.

Wrix migrates the selected file without parsing or copying its contents into
an adjacent private `<selected-file>.wrix-auth/auth.json`, retaining the selected
path as a symlink. The store is shared across repositories and containers using
that credential source; different overrides retain independent stores.
Directories are `0700` and credential files `0600`. Only that credential-only
directory is mounted read-write at `/mnt/wrix/pi-agent-auth` on either platform;
Pi's container-local `~/.pi/agent/auth.json` points into it. Neither the original
parent directory nor unrelated host Pi settings, extensions, sessions, or
credentials are exposed. Conflicting copies, non-regular credential targets,
and unexpected store entries fail closed without following a guest-controlled
auth symlink on the host.

Completed Pi writes are immediately host-backed, independent of launcher exit,
other mount synchronization, or cleanup. No session snapshot is copied back.
The shared directory also carries Pi's sibling `auth.json.lock`: Wrix's bundled
Pi enables `proper-lockfile` canonical-target resolution so file symlinks do not
produce container-local locks. Synchronous readers use the same 30-second stale
threshold as asynchronous refreshes, rather than stealing a live refresh's
lease after ten seconds. Pi re-reads and merges credentials under that
lock, including the expiry recheck and OAuth refresh; concurrent sessions reuse
the winning refresh. Pi owns lock heartbeat/stale-lock recovery and credential
write semantics; this does not promise power-loss atomicity of Pi's in-place
writes or prevent provider-side revocation.

Before the first migration, stop host Pi and containers launched by older Wrix
versions (their eventual copyback cannot be coordinated by the new launcher).
Migration serializes competing Wrix launchers, refuses an existing Pi lock, and
recovers a rename interrupted before symlink creation without replacing saved
credentials. Independently installed host Pi and `agentPkg` overrides need the
same canonical-target locking and stale thresholds to run concurrently through
symlinks; unpatched Pi versions using `realpath: false` are not safe concurrent
writers.
A host SDK can use the canonical credential path directly, but still needs
matching stale thresholds. Wrix does not modify independently installed host software. The packaged-Pi verifier
executes the real storage and Codex refresh implementation with a mock token
endpoint; the live verifier additionally exercises Podman/Apple Container
mounts and interrupted launcher shutdown.
[system](verify:security.pi-auth-storage)
[system](test-ci:test-security-pi-auth-isolation)

Acceptable because:

- The container runs as a single identity (on the default Linux boundary,
  rootless container-root — kernel uid 0 inside a user namespace owned by the
  unprivileged host user; claude runs with `IS_SANDBOX=1` so it accepts that
  root. The microVM path is equivalent — krun maps the host user to root and
  uses `LD_PRELOAD` libfakeuid). There is no second principal, so the only
  processes that can read `/proc` are ones the agent itself started.
- The credential is already present in the operator's environment,
  `SpawnConfig`, or an on-host auth file; passing it through adds no new
  host-side exposure.
- Credentials may be session-scoped tokens, OAuth material, or long-lived
  provider API keys. Wrix treats all such credentials as opaque: the launcher
  neither inspects nor enforces their lifetime, and sandbox exit does not revoke
  a credential the agent obtained.
  Long-lived keys therefore have a blast radius beyond one session. Accepting
  that risk relies on operator use of available provider controls:
  least-privilege scopes, spending budgets, rate limits, rotation, and prompt
  revocation after suspected exposure.

A secrets-file mount (`/run/secrets/oauth_token`) would prevent
`/proc/environ` exposure but adds complexity for marginal benefit
against the stated threat model. wrix does not model providers or keys
itself — provider/model defaults and the agent's own credential resolution are
the agent's concern. Wrix validates only runtime-secret environment names and
required/optional policy, not provider semantics or key formats. Pi gets
image-baked non-secret `settings.json` defaults and a runtime `auth.json` mount;
its project session persistence uses an explicit `sessionDir`, not a broad
import of `~/.pi/agent`. Claude gets its settings surface. Wrix only delivers
the secret into the container.

### Network Exfil Baseline

The sandbox network modes and always-on isolation baseline are owned by
`sandbox.md`. This spec relies on that contract and defines only the
credential-exfiltration rubric for allowlist membership.

The **base allowlist** every profile inherits for `limit` mode is enumerated
by `profiles.md`; this spec owns the *rubric* the membership must satisfy.
Each base-allowlist entry must either pair with a specific credential (the
exfil risk is accepted for the agent autonomy that credential enables) or be
credentialless (the exfil risk is bounded by what an anonymous request can
leak). The entry-to-domain mapping stays in `profiles.md` so the allowlist has
a single source of truth.

Per-profile additions must satisfy the same rubric and are owned by
`profiles.md`. Network mechanics
(`open` vs `limit`, in-sandbox firewall setup, exact local endpoint exceptions,
DNS exceptions, one-time startup domain resolution, IPv6 policy, and fail-closed
`capsh` capability drop) are owned by `sandbox.md`. Darwin host `pf` is not part
of the sandbox network contract.

### Audit Trail

Policy-leakage detection is anchored in the selected **agent's own session
transcript** when the runtime produces one. Claude uses
`/workspace/.claude/`, Pi uses `/workspace/.pi/agent/sessions`, and an external
direct runner owns any transcript it persists under its `/workspace` session
root. Wrix's placeholder direct runner does not synthesize intent or reasoning
content. At every session termination, including setup failure and signals,
wrix writes a **session-metadata index** to a collision-resistant,
timestamp-prefixed JSON file under `/workspace/.wrix/log/` containing:

- `timestamp_start`, `timestamp_end`, `duration_seconds`
- `exit_code`, `mode`
- `bead_id` (read from the mounted orchestrator `SpawnConfig`; null in
  non-orchestrated sessions)
- `wrix_session_id`, `claude_session_id`
- `agent_session_dir` — pointer to the selected runtime's session root

The index is the **audit anchor**; a runtime-provided agent transcript is the
**audit content**. Together they are Wrix's authoritative security audit
surface for transcript-producing runtimes. The index remains authoritative
session metadata when a placeholder direct runtime produces no transcript. It
is the smallest artefact that makes a session findable post-hoc (by bead, by
time, by exit code); only a real agent transcript is rich enough to reason
about what the agent intended at each step.

Wrix deliberately does not synthesize a parallel global syscall-level or
tool-call-level audit log. When the selected runtime provides a transcript, it
already contains intent, reasoning, and outcome at the granularity that matters
for policy leakage;
OS-level audit (strace, process tree) would only add value against an
adversarial agent that hides its actions from its own transcript — a different
threat class.

Individual components may expose focused diagnostic artifacts as an explicit,
default-off feature that the operator enables by configuring a destination.
These artifacts supplement component debugging; they are not authoritative
Wrix audit content. Wrix does not automatically enable them, index them under
`.wrix/log/`, synthesize them with other tool calls, or aggregate them into its
security audit surface.

The component spec owns its diagnostic configuration, record format, emission
behavior, and disclosure of secret-bearing content. The operator who enables
the feature owns the configured destination and resulting artifacts, including
access control, retention, and deletion.

### Component-Specific Security (Cross-References)

The following security-relevant concerns are owned by sibling specs; this
section is a reference index only.

- **Boundary class and network mechanics** — `sandbox.md`
- **Base and per-profile network allowlists** — `profiles.md`
- **In-container Nix build policy** — `image-builder.md`
- **Builder credentials and trust** — `linux-builder.md`
- **Project Nix cache boundary** — `services.md`
- **Host repository Git bootstrap** — `cli.md`
- **Unsafe host container-runtime control** — `sandbox.md`
- **tmux component diagnostics** — `tmux-mcp.md`

## Success Criteria

- When the launcher's environment sets `WRIX_DEPLOY_KEY` and
  `WRIX_SIGNING_KEY` to existing files outside
  `$HOME/.ssh/deploy_keys/`, the child container observes both env
  vars and files at the sandbox-owned in-container destinations, and
  `git commit` in the child produces a commit whose `git cat-file -p HEAD`
  output contains a
  non-empty `gpgsig` field.
  [system](test-ci:test-security-nested-key-propagation)
- A fresh spawned sandbox configures global `user.name` / `user.email`,
  installs pinned GitHub host keys at `/etc/ssh/ssh_known_hosts`, uses
  the mounted deploy key with strict host-key checking for GitHub SSH,
  makes an empty signed commit, and verifies that commit as a good SSH
  signature without manual `ssh-keyscan` or `git config`.
  [system](test-ci:test-security-git-ssh-bootstrap)
- Wrix-initialized host Git, container Git, and Loom's independent
  `.loom/integration` clone all use context-resolved repo deploy/signing keys,
  strict pinned GitHub host-key verification, and no ambient user SSH
  identities; a fresh host-side GitHub SSH operation reaches authentication or
  repository authorization without host-key verification failure.
  [system](test-ci:test-security-host-container-loom-git-helper)
- When `WRIX_DEPLOY_KEY` or `WRIX_SIGNING_KEY` is set in the
  launcher's environment but the pointed-at file does not exist, the
  launcher exits non-zero with a stderr message naming the missing
  path, before the container is started.
  [test](../crates/wrix-sandbox/tests/launch.rs::missing_key_env_paths_fail_before_container_start)
- Under `wrix spawn`, when the deploy key does not resolve (no env pointer and
  no `$HOME/.ssh/deploy_keys/` fallback), the launcher exits non-zero before
  the container starts. An unresolved signing key does the same unless
  `WRIX_GIT_SIGN=0` explicitly disables signing. Both failures name the
  unresolved key, while interactive `wrix run` permits the no-mount case.
  [test](../crates/wrix-sandbox/tests/launch.rs::spawn_requires_resolved_keys_but_run_allows_missing_keys)
- Every sandbox session, including setup failures and signal interruption,
  contributes exactly one collision-resistant session-metadata index under
  `/workspace/.wrix/log/`; all fields have their documented types,
  orchestrator-provided `bead_id` is preserved, `agent_session_dir` resolves to
  an existing directory, and same-workspace sessions starting within one UTC
  second retain distinct files.
  [system](test-ci:test-security-audit-trail-anchor)
- Explicit, default-off component diagnostics remain separate from the agent
  transcript and session-metadata index: they are not automatically enabled,
  indexed, synthesized, or aggregated as authoritative Wrix audit content, and
  their component/operator ownership boundary is explicit.
  [judge](../tests/judges/security.sh#test_scoped_component_diagnostics_policy)
- Host provider credentials declared through `runtimeSecrets` reach the selected runtime while `ProfileConfig` and assembled image content contain no secret values
  [system](test-ci:test-security-provider-credential-env)
- Launcher dry-run output identifies built-in provider credential env names but redacts their values
  [test](../crates/wrix-sandbox/tests/launch.rs::host_provider_credentials_reach_run_environment)
- A custom provider name declared through `runtimeSecrets` resolves from the host environment and is redacted in launcher dry-run output
  [test](../crates/wrix-sandbox/tests/launch.rs::declared_custom_runtime_secret_reaches_run_environment)
- A missing `"required"` runtime-secret source fails before the container starts
  [test](../crates/wrix-sandbox/tests/launch.rs::required_runtime_secret_fails_before_container_start)
- `profile.env`, `profile.hostEnv`, `mkSandbox.env`, and agent-settings env
  reject malformed names and known provider credential names, and
  `runtimeSecrets` rejects invalid names or policies at Nix evaluation
  [check](verify:sandbox.mksandbox-api)
- `ProfileConfig.profile.env` and `SpawnConfig.env` parse environment names into
  validated identifiers and reject malformed names before launch-plan
  construction
  [test](../crates/wrix-sandbox/tests/spawn_config.rs::invalid_environment_names_fail_before_launch)
- A declared runtime secret supplied through `SpawnConfig.env` satisfies required-source policy and is redacted in launcher dry-run output
  [test](../crates/wrix-sandbox/tests/spawn_config.rs::provider_credentials_in_spawn_config_are_redacted)
- Both platforms expose only durable Pi credential storage, preserve completed
  writes before exit, share refreshes across overlapping containers and
  repositories, and retain credentials after killed launchers and restarts.
  [system](test-ci:test-security-pi-auth-isolation)
- Bundled Pi locks the shared symlink target, performs only one concurrent Codex
  refresh, keeps synchronous readers from stealing live refresh locks, merges
  other provider updates, and recovers abandoned locks without
  reverting saved credentials.
  [system](verify:security.pi-auth-storage)
- Pi migration preserves selected credentials with restrictive permissions
  without exposing sibling files.
  [test](command::launch::pi_auth::test::migration_preserves_credentials_and_isolates_siblings)
- Interrupted Pi migration recovers the saved credential file rather than
  initializing or copying over it.
  [test](command::launch::pi_auth::test::interrupted_migration_recovers_without_overwriting_credentials)
- Host preparation rejects guest-controlled auth symlinks without reading or
  changing their targets.
  [test](command::launch::pi_auth::test::unsafe_shared_storage_is_rejected_without_touching_symlink_target)
- A transcript-producing built-in agent, or an external direct runner that
  persists its own transcript, provides fit-for-purpose audit content for the
  stated policy-leakage threat model without claiming adversarial-agent
  detection or content from Wrix's placeholder direct runner.
  [judge](../tests/judges/security.sh#test_agent_transcript_audit_fit)

## Requirements

### Functional

1. **Host-source resolution precedence** — launcher resolves each
   key's host source by env-first, `$HOME/.ssh/deploy_keys/`-second;
   independently per key; fails loud if env is set but file does not
   exist. Under `wrix spawn`, an unresolved deploy key is fail-loud, as is an
   unresolved signing key unless `WRIX_GIT_SIGN=0`; interactive `run` permits
   the no-mount fall-through. (See *Credential Surfaces*.)
2. **In-container key delivery** — after host-source resolution, the launcher
   uses the validation, mount, and child-environment contract owned by
   `sandbox.md`. Host source paths do not cross the boundary.
3. **Platform symmetry** — Linux and macOS launchers implement the
   same precedence rule; behavior is identical across platforms
   modulo the launcher's outer shell/applescript wrapping.
4. **Host/repository Git transport** — Wrix-initialized host Git,
   container Git, and independently initialized Loom clones use
   context-resolved repo deploy/signing keys with strict pinned GitHub host-key
   verification, and fail rather than falling back to ambient user SSH
   identities or trust-on-first-use host keys.
5. **Agent credentials** — provider keys cross the boundary only through declared runtime environment delivery, while Pi auth crosses through the platform-specific file delivery described in *Credential Surfaces*. Runtime declarations serialize only validated names plus required/optional policy; neither channel contributes secret values to Nix evaluation, `ProfileConfig`, image config, or image layers.
6. **Audit anchor** — every sandbox session writes one uniquely named
   session-metadata index whose complete field set identifies the session and
   whose `agent_session_dir` points at the selected runtime's session root. For
   Claude and Pi that root contains the agent transcript; an external direct
   runner owns any transcript content under its session root.
7. **Scoped component diagnostics** — a component may emit operator-enabled,
   default-off diagnostics without changing the authoritative audit anchor.
   Wrix does not automatically index, synthesize, or aggregate those artifacts;
   the component owns their contract and the operator owns their destination,
   access control, retention, and deletion.

### Non-Functional

1. **Trust posture** — the launcher's parent process is trusted to
   choose key source paths. Validation is presence-only (`[ -f ]`);
   no path-prefix, ownership, mode, or content check.
2. **Composability** — every wrix launcher behaves identically with
   respect to keys regardless of whether its parent is a shell or
   another wrix container.
3. **Audit fit** — when a selected runtime produces an agent transcript, it is
   treated as fit-for-purpose audit content for the stated threat model (policy
   leakage from a misbehaving but not adversarial agent). Wrix's placeholder
   direct runner supplies metadata only.

## Out of Scope

- **Key rotation cadence.** Operator responsibility; no spec contract.
- **Additional key-path validation** (ownership checks, path-prefix
  restrictions, mode checks on parent-supplied env paths). The
  trust-posture invariant explicitly forbids these.
- **Global syscall-level or tool-call-level audit synthesis and diagnostic
  aggregation** into Wrix's security audit surface. Explicit, default-off
  component diagnostics are permitted by the scoped exception above but remain
  non-authoritative and separate from the transcript and metadata index.
- **Adversarial-agent threat model** — an agent that deliberately
  evades its own transcript. The audit-anchor invariant does not
  defend against this.
- **OAuth secrets-file mount** (`/run/secrets/oauth_token`). Env
  passthrough is the chosen mechanism.
- **Image signing** and supply-chain verification of pulled artefacts.
- **Serving the host `/nix/store` to sandboxes** via Harmonia, nix-serve, or
  equivalent host-store-backed binary-cache tools. Wrix's project cache is an
  explicit cache populated by project-scoped publish rules, not a host-store
  view.
- **Multi-tenant sharing** of a sandbox between operators.
