# Security

Cross-cutting security invariants for wrix sandboxes: threat model, credential
surfaces, network exfil baseline, audit anchor, and scoped component
diagnostics.

## Problem Statement

AI coding agents running inside wrix sandboxes may receive repository deploy
keys, commit signing keys, and agent API credentials, reach network destinations
governed by wrix's `open` / `limit` egress policy, and produce session artefacts
the operator can later inspect. The per-component specs (`sandbox.md`,
`proxy.md`, `services.md`, `cache.md`, `image-builder.md`, `linux-builder.md`,
`profiles.md`) own the _mechanics_ of each surface; this spec owns the
cross-cutting _trust model_ that ties them together — what the threat is, why
each mitigation is fit for that threat, and which invariants must hold across
components. Component-specific facts are cross-referenced, not duplicated.

## Architecture

### Threat Model

The primary realistic threat is **policy leakage**: a prompt-injected or
misbehaving agent uses its granted permissions (deploy key, network, OAuth
token, workspace access) for unintended purposes — exfiltrating code, secrets,
or credentials through legitimate channels. No kernel escape or boundary exploit
is required.

The secondary threat is **boundary escape**: the agent breaks out of the sandbox
into the host. Boundary classes and their operator controls are owned by
`sandbox.md`; this spec relies on that isolation contract.

The normal boundary model excludes host container-runtime control. The unsafe
host-Podman exception defined by `sandbox.md` can start containers with host
bind mounts and is therefore outside the normal sandbox mitigations.

Defense against policy leakage requires restricting _what's available inside the
boundary_ (network, credentials, filesystem), not strengthening the boundary
itself. The credential and network-baseline sections below are scoped to that
threat.

The audit-anchor invariant further assumes an agent that may be _misbehaving but
not adversarial_ — one that does not deliberately hide its actions from the
transcript. An adversarial-agent model (agent attempts to evade its own logging)
is a different problem class and out of scope.

### Host Workspace Authority

[Workspace Admission](workspaces.md) owns host-only association and
control-store access. Repository metadata and worker-visible exports are
observations, not host authorization. The worker mount boundary excludes native
control state, including ancestor and alias exposure, under that spec's
production launch verifier. External attestations neither issue credentials nor
authorize resource removal; operation-specific guards remain independent.

### Shared Service Trust Boundary

Acceptance:
[Shared Service and Proxy Trust Acceptance](#shared-service-and-proxy-trust-acceptance).

The host-user service is trusted infrastructure, not an agent sandbox.
`services.md` owns workspace namespace isolation and private-authority
separation; `proxy.md` owns per-launch traffic grants and host-management
separation. Logical tenancy limits ordinary client access but does not preserve
the compromise/fault isolation of separate service containers. A compromised
cohosted component can affect shared service-visible state and policy, including
Celler's delegated cache-signing authority under
[Cache Authority](cache.md#cache-authority). That authority is inside the shared
compromise boundary, not a host-private key protected from its components. The
shared sccache daemon also intentionally executes eligible worker compiler jobs
here, including procedural macros. These jobs participate in that trust boundary
rather than receiving compiler-only isolation: they can affect service-visible
Dolt/proxy/Celler state and signing authority without a worker sandbox escape.
This can undermine sandbox cache integrity even though direct worker Nix uploads
remain disabled. Host Nix does not consume this shared cache or trust its keys;
service cache signing therefore grants no Wrix-managed host substitution
authority. Removing personal-host cache sharing is not a claim of strictly
stronger isolation in every dimension. Host publishing/management credential
files, controller-held issuer private keys and authoritative manifests remain
outside its mounts. The bounded host control service owns lifecycle, fixed-role
routing and host authority under
[Host Control](services.md#host-control-service), not agent supervision,
provider credentials or general host execution. Authorized requests cross the
service boundary, so a service compromise may observe their scoped runtime
credentials; unmounted files do not imply runtime credential isolation. General
host filesystem access and the host Nix daemon remain excluded.

Service, cache and proxy client credentials authorize only their declared scope.
They are runtime delivery, not provider credentials or permission to manage the
host service. Their confidentiality boundary follows the component contracts;
traffic/pull credentials are not host-management, cache-signing or publishing
authority. Sandbox cache access does not require granting an agent
root-equivalent trusted-user authority over the host or any shared Nix daemon.

[Cache Visibility](cache.md#cache-visibility) authorizes the common
approved-public dependency view and the client's own canonical private
repository view, not an implicit union of private data or a sharing-group
service. Host management is the view and publication-audience authority: unlike
mutable repository Git policy, repository edits or agent configuration cannot
broaden cache access. Physical deduplication and knowledge of a private hash are
not read permissions; approved common reads still import into the consuming Nix
store; Celler grants do not authorize access to an installed store pool. Cache
grants are native bearer capabilities under
[Native Pull Credentials](cache.md#native-pull-credentials): one day for
launcher-renewed sandbox tokens. Host pull tokens and host-activation pull
refresh are excluded; host publication uses separate scoped authority. Current
policy constrains new issuance/renewal, not already-valid tokens; immediate
revocation and recall of imported data are not promised. Native cache expiry
does not revoke or delete artifacts already installed in a client's store.

Host Nix neither consumes the shared service cache nor trusts any of its signing
keys. Signatures authenticate accepted bytes, not correct derivation execution;
keeping signing beside delegated compiler code can poison sandbox outputs but
cannot authorize Wrix-managed host substitution. Host publication is one-way
under
[Host Publication-only Integration](cache.md#host-publication-only-integration).
Independent host upstream trust/local-store reuse remain intact. Existing
operator-managed trust requires explicit remediation, not silent machine-wide
rewrites or claims that imported artifacts have been recalled. Runtime escapes,
host-client parser bugs and executing agent-modified workspace files remain
separate host risks.

[Tool Cache Data Boundary](profiles.md#tool-cache-data-boundary) also separates
built-in sandbox Cargo registry/git, sccache and uv data from personal host
caches: no default bind, prewarm copy or write-back. This prevents those
defaults from reintroducing a host/agent data bridge independently of Celler.
[Shared Tool Cache Modes](profiles.md#shared-and-private-tool-cache-modes)
deliberately accept one cross-repository worker cache trust domain when sharing
is enabled, even across different credential grants. Private cached sources and
compiled contents may be read or influenced by peers; locks/checksums do not
supply repository authorization or build attestation. Runtime credential files
and host/service management authority are not shared cache backing.

The coarse opt-out keeps all applicable tool caches fresh and private for that
launch without pool reads/writes/prewarms or host fallback. It neither recalls
previously shared contents nor sanitizes arbitrary workspace data or explicit
caller mounts. Selective-source privacy and tracking private code into compiled
cache outputs are deferred. Ordinary Celler API audiences and host trust
configuration remain unchanged; this is not a worker Nix-upload feature. Private
tool mode does not protect Celler-private data or other service state against
code already executing in the privileged shared compiler boundary.

Tool-cache sharing can be remembered in repository `wrix.toml` or selected for
one launch. Explicit launch selection wins in both directions, including
enabling sharing over a private repository default; an invocation does not
rewrite that policy. As with other trusted repository launch input, workspace
writers can change future inherited choices. This is not an immutable
host-enforced privacy restriction, an extra environment policy channel or a
change to Celler authority.

Full agent/Cargo runtimes remain in their existing worker boundaries; selected
compiler work is the explicit service-side exception under
[Shared Rust Compiler](profiles.md#shared-rust-compiler-service). Its CPU/memory
use belongs to the service runtime budget, not automatically the worker's
limits. Restricted Rust requires private/local tool caches and receives no
shared compiler execution route; this prevents its own compiler code bypassing
the worker's network policy. Proxy guarantees still assume the shared service is
trusted, not compromised by another cohosted compiler job. Full worker runtime
migration remains excluded. Separately admitted service-owned Nix execution and
its cooperative pool boundary belong to
[Service-Owned Nix Stores](store.md#execution-and-confidentiality), including
strict build isolation, public-network-only execution and no host substitution
authority. Acceptance: [Store Execution](store.md#execution-acceptance) and
[Store Pools](store.md#pool-acceptance).

### Credential Surfaces

The agent credential surfaces include the deploy key, the commit signing key,
and the agent's API credentials (claude's OAuth token, or a non-claude agent's
provider API key / auth file). Scoped infrastructure credentials follow
[Shared Service Trust Boundary](#shared-service-trust-boundary); cache signing
state is not an agent credential.

#### Deploy & Signing Keys

Both keys are ed25519, generated **passphraseless** so the agent can use them
non-interactively. Acceptable because:

- The host-side directory holding them (`~/.ssh/deploy_keys/`) is mode `700`;
  the keys themselves are `600`.
- The deploy key is **repository-scoped** — GitHub deploy keys grant write only
  to the single repository they were added to. Compromise blast radius is one
  repo, not the user's full GitHub identity.
- The signing key adds attribution to commits; it does not grant any additional
  access.

**Explicit grants.** `cli.md` owns `[git]` policy and `sandbox.md` owns launch
overrides. Neither `run` nor `spawn` receives deploy or signing keys without the
corresponding effective grant. Grants are independent: signing alone does not
grant repository access, and deploying alone does not grant signing. A deploy
grant exposes the key's actual repository permissions; it is not a read-only Git
capability or a validated publishing transaction.

**Repository policy trust.** The repository's `wrix.toml` is trusted launch
input, not agent-resistant authorization storage. Because the workspace is
writable, an agent can edit that policy and change grants on subsequent launches
that inherit it. Explicit caller overrides still take precedence. Grants control
Wrix-managed key delivery; they do not protect policy from workspace writers.

**Host-source resolution precedence.** Only after a key is granted does the
launcher resolve its _host_ source path. The rule below is stated for the deploy
key; the signing key follows the same rule with `WRIX_SIGNING_KEY` and
`$HOME/.ssh/deploy_keys/<name>-signing` substituted in. The two keys are
resolved independently — neither affects the other.

1. If `WRIX_DEPLOY_KEY` is set in the launcher's environment, it is the only
   candidate. Use it if it names an existing file; otherwise fail before service
   or container startup, naming the key and invalid source path.
2. Only when `WRIX_DEPLOY_KEY` is unset, use `$HOME/.ssh/deploy_keys/<name>` if
   that file exists.
3. If that fallback is also absent, fail before service or container startup,
   naming the granted but unresolved key.

An ungranted key is not resolved, staged, mounted, or advertised through a child
key-path environment variable, even if its host fallback exists or an ambient
pointer is set. A host pointer selects a source; it is not a grant. Provider
credentials remain subject to their separate policy below.

The same rules apply to `run` and `spawn`, including nested launches. A trusted
parent may supply key source pointers for a child, but the child still needs an
effective grant. Wrix does not inherit publishing authority merely because the
caller is an orchestrator.

**In-container delivery is fixed.** `sandbox.md` owns deploy-key-name
validation, mount destinations, and the child environment values. The security
invariant is that the selected host source path never crosses the boundary and
every launch applies that same delivery contract, which makes nested wrix
launches recursively composable.

**Host and repository Git bootstrap.** `cli.md` owns the `wrix init repo`
command that applies repo-local Git config for host shells, devshells,
containers, and Loom clones when invoked in each repository. The security
invariant is that Wrix-managed Git transport uses only the context-resolved
deploy key: `WRIX_DEPLOY_KEY` when the launcher supplied an in-container path,
or `$HOME/.ssh/deploy_keys/<name>` on the host. If neither exists, the operation
fails instead of trying the user's default SSH identities, SSH agent keys, or
`~/.ssh/config` identities.

All Wrix-managed GitHub SSH uses strict, noninteractive host-key verification
with Wrix-pinned GitHub host keys. `StrictHostKeyChecking=no`, runtime
`ssh-keyscan` trust-on-first-use, and appending learned GitHub keys to the
user's `~/.ssh/known_hosts` are outside the security model. When Wrix creates
SSH directories or compatibility config/known-hosts files, directory modes are
`0700` and `config` / `known_hosts` file modes are `0600`.

Commit signing follows the same context-resolution rule through the signing key
(`WRIX_SIGNING_KEY` or `$HOME/.ssh/deploy_keys/<name>-signing`) when enabled
under `cli.md` policy. Missing requested signing material is a hard failure.
Inside a sandbox, the effective sign grant controls default Git signing even if
the mounted repository has different host signing configuration. This is an
execution-local override: it does not rewrite the repository's shared Git
configuration or affect concurrent host sessions.

**Trust model.** The launcher's parent process is trusted to choose the host
source path. The launcher's only validation is presence (`[ -f ]`); it performs
no path-prefix, ownership, mode, or content check. The justification is symmetry
with the existing surface: a hostile parent could already write to
`$HOME/.ssh/deploy_keys/`, so accepting env pointers adds no new attack surface.

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

Wrix migrates the selected file without parsing or copying its contents into an
adjacent private `<selected-file>.wrix-auth/auth.json`, retaining the selected
path as a symlink. The store is shared across repositories and containers using
that credential source; different overrides retain independent stores.
Directories are `0700` and credential files `0600`. Only that credential-only
directory is mounted read-write at `/mnt/wrix/pi-agent-auth` on either platform;
Pi's container-local `~/.pi/agent/auth.json` points into it. Neither the
original parent directory nor unrelated host Pi settings, extensions, sessions,
or credentials are exposed. Conflicting copies, non-regular credential targets,
and unexpected store entries fail closed without following a guest-controlled
auth symlink on the host.

Completed Pi writes are immediately host-backed, independent of launcher exit,
other mount synchronization, or cleanup. No session snapshot is copied back. The
shared directory also carries Pi's sibling `auth.json.lock`: Wrix's bundled Pi
enables `proper-lockfile` canonical-target resolution so file symlinks do not
produce container-local locks. Synchronous readers use the same 30-second stale
threshold as asynchronous refreshes, rather than stealing a live refresh's lease
after ten seconds. Pi re-reads and merges credentials under that lock, including
the expiry recheck and OAuth refresh; concurrent sessions reuse the winning
refresh. Pi owns lock heartbeat/stale-lock recovery and credential write
semantics; this does not promise power-loss atomicity of Pi's in-place writes or
prevent provider-side revocation.

Before the first migration, stop host Pi and containers launched by older Wrix
versions (their eventual copyback cannot be coordinated by the new launcher).
Migration serializes competing Wrix launchers, refuses an existing Pi lock, and
recovers a rename interrupted before symlink creation without replacing saved
credentials. Independently installed host Pi and `agentPkg` overrides need the
same canonical-target locking and stale thresholds to run concurrently through
symlinks; unpatched Pi versions using `realpath: false` are not safe concurrent
writers. A host SDK can use the canonical credential path directly, but still
needs matching stale thresholds. Wrix does not modify independently installed
host software. The packaged-Pi verifier executes the real storage and Codex
refresh implementation with a mock token endpoint; the live verifier
additionally exercises Podman/Apple Container mounts and interrupted launcher
shutdown. [system](verify:security.pi-auth-storage)
[system](test-ci:test-security-pi-auth-isolation)

Acceptable because:

- The container runs as a single identity (on the default Linux boundary,
  rootless container-root — kernel uid 0 inside a user namespace owned by the
  unprivileged host user; claude runs with `IS_SANDBOX=1` so it accepts that
  root. VM boundaries retain real guest root, matching their workspace and Nix
  ownership; there is no UID-spoofing preload or Darwin UID rewrite). There is
  no second principal, so the only processes that can read `/proc` are ones the
  agent itself started.
- The credential is already present in the operator's environment,
  `SpawnConfig`, or an on-host auth file; passing it through adds no new
  host-side exposure.
- Credentials may be session-scoped tokens, OAuth material, or long-lived
  provider API keys. Wrix treats all such credentials as opaque: the launcher
  neither inspects nor enforces their lifetime, and sandbox exit does not revoke
  a credential the agent obtained. Long-lived keys therefore have a blast radius
  beyond one session. Accepting that risk relies on operator use of available
  provider controls: least-privilege scopes, spending budgets, rate limits,
  rotation, and prompt revocation after suspected exposure.

A secrets-file mount (`/run/secrets/oauth_token`) would prevent `/proc/environ`
exposure but adds complexity for marginal benefit against the stated threat
model. wrix does not model providers or keys itself — provider/model defaults
and the agent's own credential resolution are the agent's concern. Wrix
validates only runtime-secret environment names and required/optional policy,
not provider semantics or key formats. Pi gets image-baked non-secret
`settings.json` defaults and a runtime `auth.json` mount; its project session
persistence uses an explicit `sessionDir`, not a broad import of `~/.pi/agent`.
Claude gets its settings surface. Wrix only delivers the secret into the
container.

### Network Exfil Baseline

The sandbox network modes and always-on isolation baseline are owned by
`sandbox.md`. This spec relies on that contract and defines only the
credential-exfiltration rubric for allowlist membership.

The **base allowlist** every profile inherits for `limit` mode is enumerated by
`profiles.md`; this spec owns the _rubric_ the membership must satisfy. Each
base-allowlist entry must either pair with a specific credential (the exfil risk
is accepted for the agent autonomy that credential enables) or be credentialless
(the exfil risk is bounded by what an anonymous request can leak). The
entry-to-host/port mapping stays in `profiles.md` so the allowlist has a single
source of truth.

Per-profile additions must satisfy the same rubric and are owned by
`profiles.md`. Network modes, guest firewall, exact local endpoints, IPv6 policy
and fail-closed capability drop belong to `sandbox.md`; authenticated restricted
egress and request-time DNS/connect admission belong to `proxy.md`. Darwin host
`pf` is not part of the sandbox contract.

Acceptance:
[Shared Service and Proxy Trust Acceptance](#shared-service-and-proxy-trust-acceptance).

Without TLS inspection, proxy authority admission is not enforcement of
encrypted application identity or content. Allowlist membership accepts the
remaining leakage risk, including approved-service exfiltration and shared-IP
virtual hosting; it does not promise an exfiltration-proof environment.

### Audit Trail

The selected **agent's own session transcript**, when present, is evidence for
investigating policy leakage. Claude uses `/workspace/.claude/`, Pi uses
`/workspace/.pi/agent/sessions`, and a consumer direct runner owns any
transcript it persists. Wrix does not synthesize conversation content for
runtimes that provide none.

The host launcher writes an **execution-metadata index** after validating the
launch inputs and before starting workspace services, container setup, or agent
work. Each execution gets one collision-resistant, timestamp-prefixed JSON file
under the selected workspace's `.wrix/log/`, visible inside the container as
`/workspace/.wrix/log/`. Failure to establish this initial record prevents
execution rather than allowing unindexed work.

The record contains:

- `execution_id` — unique string for this launch attempt, not a conversation or
  terminal identity
- `state` — `"incomplete"` until the launcher's terminal outcome is recorded,
  then `"completed"`, whether the attempt succeeded or failed
- `timestamp_start` — UTC timestamp string; `timestamp_end` and
  `duration_seconds` are null until completion, then a UTC timestamp and a
  nonnegative number
- `exit_code` — integer exit code observed from the foreground container-runtime
  command (`podman run` or Apple `container run`), otherwise null
- `signal` — signal name, such as `SIGTERM`, when the launcher observes that
  runtime command terminated by a signal, otherwise null
- `mode` — `"run"` or `"spawn"`; `agent_kind` — the selected runtime kind
- `bead_id` — orchestrator-provided issue identifier or null
- `agent_session_id` — runtime-reported conversation identifier or null
- `agent_session_dir` — session-root absolute path in the container's filesystem
  when known, otherwise null; `/workspace/...` maps to the selected host
  workspace, not the host's literal `/workspace` directory
- `focus_target` — the optional opaque terminal target used by the notification
  system, or null; never used as an execution or conversation identifier

Notification navigation uses only host-owned terminal registrations. Payloads
carry opaque lookup keys, not executable commands or host socket/window routes.
Delivery never moves focus; the operator invokes `wrix-focus-notification` to
visit pending attention. The unauthenticated Darwin transport permits spoofed
attention for known keys, but cannot introduce a new host navigation route.
`notifications.md` owns queue behavior and platform limitations.
[system](verify:notifications.attention-navigation)

Completion updates the same file atomically. `"completed"` means the launcher
recorded the end of its attempt, not that the attempt succeeded or that Wrix
independently verified every container process stopped. A setup/launch failure
before a container-runtime status is available completes the record with null
`exit_code` and `signal`; the launcher still reports the failure.

Exit metadata describes that runtime command, not a separately observed agent
process. An ordinary numeric exit such as `137` remains an exit code; Wrix does
not infer a signal from `128 + signal-number`. Known session roots use container
coordinates consistently, including for direct runners; a path outside
`/workspace` requires its declared mount mapping for host access.

In both launch modes, an unavailable completion update produces a stderr
diagnostic and leaves the record incomplete. It does not replace an observed
runtime status with a metadata error: successful and failed runtime outcomes
retain their caller-visible status. If launch failed before a runtime status was
available, the original launch failure remains caller-visible.

Concurrent executions never overwrite each other's records. Optional
conversation fields are populated only from information attributable to that
execution, not the latest unrelated transcript in a shared directory.

An incomplete record means completion was not recorded, not that the process is
definitely running or definitely stopped. A killed launcher or host failure may
leave it incomplete permanently; Wrix does not fabricate an exit status, claim a
shell trap handles every failure, or automatically recover that work. No
independent environment lifecycle or environment identity is introduced.

Optional [trusted execution tickets](sandbox.md#trusted-execution-tickets) are a
separate host-only containment protocol. They do not promote this worker-visible
audit index into lifecycle or disposal authority. Their explicit reconciliation
is not automatic recovery of incomplete audit records.

The index is the **audit anchor** and a runtime transcript is the available
**audit content**. Neither is an exhaustive audit: codemode may expose only a
selected summary while intermediate tool results and side effects remain outside
the recorded content. Missing transcript evidence is not evidence that an action
did not occur. Wrix does not synthesize a parallel global syscall log,
nested-tool recorder, or diagnostic aggregator.

Individual components may expose focused diagnostic artifacts as an explicit,
default-off feature that the operator enables by configuring a destination.
These artifacts supplement component debugging; they are not authoritative Wrix
audit content. Wrix does not automatically enable them, index them under
`.wrix/log/`, synthesize them with other tool calls, or aggregate them into its
security audit surface.

The component spec owns its diagnostic configuration, record format, emission
behavior, and disclosure of secret-bearing content. The operator who enables the
feature owns the configured destination and resulting artifacts, including
access control, retention, and deletion.

### Component-Specific Security (Cross-References)

The following security-relevant concerns are owned by sibling specs; this
section is a reference index only.

- **Boundary class and network mechanics** — `sandbox.md`
- **Base and per-profile network allowlists** — `profiles.md`
- **Restricted proxy admission and TLS limits** — `proxy.md`
- **Host-user service tenancy and managed data** — `services.md`
- **In-container Nix build policy** — `image-builder.md`
- **Builder credentials and trust** — `linux-builder.md`
- **Project Nix cache boundary** — `cache.md`
- **Host repository Git bootstrap** — `cli.md`
- **Unsafe host container-runtime control** — `sandbox.md`

## Success Criteria

- When both Git grants are explicitly enabled and the launcher's environment
  sets `WRIX_DEPLOY_KEY` and `WRIX_SIGNING_KEY` to existing files outside
  `$HOME/.ssh/deploy_keys/`, the child container observes both env vars and
  files at the sandbox-owned in-container destinations, and `git commit` in the
  child produces a commit whose `git cat-file -p HEAD` output contains a
  non-empty `gpgsig` field.
  [system](test-ci:test-security-explicit-nested-key-grants)
- A fresh spawned sandbox with both Git grants explicitly enabled configures
  `user.name` / `user.email`, installs pinned GitHub host keys at
  `/etc/ssh/ssh_known_hosts`, uses the mounted deploy key with strict host-key
  checking for GitHub SSH, makes an empty signed commit, and verifies that
  commit as a good SSH signature without manual `ssh-keyscan` or `git config`.
  [system](test-ci:test-security-explicit-git-ssh-bootstrap)
- Wrix-initialized host Git, container Git, and Loom's independent
  `.loom/integration` clone all use context-resolved repo deploy/signing keys,
  strict pinned GitHub host-key verification, and no ambient user SSH
  identities; a fresh host-side GitHub SSH operation reaches authentication or
  repository authorization without host-key verification failure.
  [system](test-ci:test-security-host-container-loom-git-helper)
- Linux live-test images provide the production context-resolved Git helper, and
  their packaged tools can initialize, sign, and verify a repository.
  [system](test-ci:test-image-git-helper-parity)
- A granted deploy or signing key with a missing explicit source fails even when
  a valid fallback exists; with no explicit source, a missing fallback also
  fails. Both failures name the unresolved key before services or containers
  start, identically for run and spawn
  [test](../crates/wrix-sandbox/tests/launch.rs::granted_git_keys_are_required_before_startup)
- With no grants, both launch modes succeed without Git keys and do not resolve
  or stage ambient key sources; all four deploy/sign combinations deliver
  exactly the granted keys independently
  [test](../crates/wrix-sandbox/tests/launch.rs::git_key_grants_are_independent_and_ambient_sources_do_not_grant)
- Security documentation identifies repository Git policy as trusted, mutable
  launch input whose modification can change grants on subsequent launches; it
  does not describe key-delivery grants as an agent-resistant authorization
  boundary
  [judge](../tests/judges/security-credentials.sh#test_repository_git_policy_trust_boundary)
- Live Linux and Darwin sandbox launches expose exactly the granted private keys
  and corresponding child environment paths, including signing-only and no-key
  launches when host keys exist
  [system](test-ci:test-security-git-grant-isolation)
- Effective sandbox signing policy overrides mounted host signing settings for
  container Git only, permits unsigned commits with sign disabled, and leaves
  shared repository config unchanged
  [system](test-ci:test-security-session-local-signing)
- Each validated launch establishes one correctly typed execution record before
  any service/container startup, preserves optional issue and focus identities,
  and keeps concurrent execution IDs and files distinct
  [test](../crates/wrix-sandbox/tests/execution_metadata.rs::launch_records_precede_work_and_do_not_collide)
- Concurrent same-second execution IDs and files remain distinct
  [test](command::launch::execution_metadata::test::concurrent_same_timestamp_records_remain_distinct)
- A record-creation failure prevents work; observed success, failure, or signal
  termination atomically completes the same record without inventing a
  conversation identity
  [test](../crates/wrix-sandbox/tests/execution_metadata.rs::observed_termination_updates_the_original_record)
- In both launch modes, a completion-update failure leaves the record incomplete
  and emits a stderr diagnostic without replacing the observed runtime status or
  the original pre-runtime launch failure
  [test](../crates/wrix-sandbox/tests/execution_metadata.rs::completion_update_errors_preserve_launch_outcome)
- Exit metadata records the foreground runtime command's observed exit code or
  signal without inferring agent status or decoding ordinary `128 + n` exit
  codes as signals; failures without a runtime status complete with null status
  fields rather than claiming success
  [test](../crates/wrix-sandbox/tests/execution_metadata.rs::metadata_status_tracks_the_container_runtime_command)
- Known session roots are recorded as container-absolute paths for all agent
  kinds, with `/workspace` mapped to the selected host workspace; unavailable
  roots remain null rather than mixing host and container coordinates
  [test](../crates/wrix-sandbox/tests/execution_metadata.rs::session_roots_use_container_absolute_paths)
- Live packaged launches retain an incomplete record after an abruptly killed
  launcher and complete records for observed startup failures and normal exits;
  agent-specific conversation information cannot leak between executions
  [system](test-ci:test-security-execution-metadata-lifecycle)
- Explicit, default-off component diagnostics remain separate from the agent
  transcript and execution-metadata index: they are not automatically enabled,
  indexed, synthesized, or aggregated as authoritative Wrix audit content, and
  their component/operator ownership boundary is explicit.
  [judge](../tests/judges/security.sh#test_scoped_component_diagnostics_policy)
- Host provider credentials declared through `runtimeSecrets` reach the selected
  runtime while `ProfileConfig` and assembled image content contain no secret
  values [system](test-ci:test-security-provider-credential-env)
- Production `run --dry-run` human and JSON reports identify built-in provider
  credential names without exposing their environment values in reports or
  diagnostics.
  [test](../crates/wrix-cli/tests/preflight.rs::dry_run_redacts_builtin_provider_credentials)
- Production `run --dry-run` resolves a custom `runtimeSecrets` declaration from
  the host environment and reports its name/availability without exposing its
  value in either rendering or diagnostics.
  [test](../crates/wrix-cli/tests/preflight.rs::dry_run_redacts_custom_runtime_secrets)
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
- Production `spawn --dry-run` recognizes a declared runtime secret supplied
  through `SpawnConfig.env` as satisfying required-source policy without
  exposing its value in human/JSON reports or diagnostics.
  [test](../crates/wrix-cli/tests/preflight.rs::spawn_dry_run_redacts_declared_secret_overrides)
- Both platforms expose only durable Pi credential storage, preserve completed
  writes before exit, share refreshes across overlapping containers and
  repositories, and retain credentials after killed launchers and restarts.
  [system](test-ci:test-security-pi-auth-isolation)
- Bundled Pi locks the shared symlink target, performs only one concurrent Codex
  refresh, keeps synchronous readers from stealing live refresh locks, merges
  other provider updates, and recovers abandoned locks without reverting saved
  credentials. [system](verify:security.pi-auth-storage)
- Pi migration preserves selected credentials with restrictive permissions
  without exposing sibling files.
  [test](command::launch::pi_auth::test::migration_preserves_credentials_and_isolates_siblings)
- Interrupted Pi migration recovers the saved credential file rather than
  initializing or copying over it.
  [test](command::launch::pi_auth::test::interrupted_migration_recovers_without_overwriting_credentials)
- Host preparation rejects guest-controlled auth symlinks without reading or
  changing their targets.
  [test](command::launch::pi_auth::test::unsafe_shared_storage_is_rejected_without_touching_symlink_target)
- Audit documentation and metadata distinguish execution from conversation and
  focus identities, treat absent completion as unknown, and describe transcripts
  as non-exhaustive evidence without promising intermediate codemode outcomes,
  automatic recovery, or adversarial-agent detection
  [judge](../tests/judges/security-execution.sh#test_execution_and_transcript_evidence_limits)

### Shared Service and Proxy Trust Acceptance

- Security documentation distinguishes namespace authorization from the shared
  compromise/fault boundary, explicitly places Celler cache-signing authority
  within that boundary, and separates ordinary traffic/pull delivery from
  signing/publishing/issuer/host management. It explicitly treats delegated
  compiler/procedural-macro code as service-trusted execution that can affect
  service signing/data and sandbox-cache integrity while excluding host
  shared-cache consumption, not compiler-only isolation or a universal security
  improvement. It distinguishes service versus worker resource budgets,
  restricted-Rust local compilation and the trusted-service assumption; private
  tool mode is not protection from privileged service code.
  [judge](../tests/judges/security-proxy-trust.md#shared-service-boundary)
- Cache trust documentation distinguishes the common approved-public audience
  from canonical private contents, keeps authority outside mutable repository
  policy/agent requests, and states native one-day sandbox bearer residual
  exposure after authorization withdrawal, without group-service claims. It also
  distinguishes signed acceptance from correct build execution and keeps every
  shared-service view/key outside host trust, including within one repository,
  with one-way host publication and honest legacy-trust remediation. Built-in
  tool caches likewise exclude personal-host bind/prewarm/write-back channels
  while disclosing enabled-mode cross-repository peer-data/artifact trust, the
  all-cache private opt-out and its no-recall/non-sanitization limits. It does
  not claim safety for arbitrary caller mounts or selective-source privacy. It
  treats repository tool-sharing policy as an editable default, with explicit
  invocation choice taking precedence in both directions without policy writes.
  [judge](../tests/judges/security-proxy-trust.md#cache-sharing-authority)
- The allowlist rubric accepts credential/credentialless leakage risks without
  claiming encrypted identity/content enforcement or prevention of exfiltration
  through approved services.
  [judge](../tests/judges/security-proxy-trust.md#proxy-enforcement-limits)

## Requirements

### Functional

1. **Granted key resolution** — only granted keys are resolved, independently,
   by env-first then `$HOME/.ssh/deploy_keys/` fallback. Every requested key is
   required; unrequested keys are absent even when ambient sources exist. Run
   and spawn obey the same rules in _Credential Surfaces_.
2. **In-container key delivery** — after host-source resolution, the launcher
   uses the validation, mount, and child-environment contract owned by
   `sandbox.md`. Host source paths do not cross the boundary.
3. **Platform symmetry** — Linux and macOS launchers implement the same
   precedence rule; behavior is identical across platforms modulo the launcher's
   outer shell/applescript wrapping.
4. **Host/repository Git transport** — Wrix-initialized host Git, container Git,
   and independently initialized Loom clones use context-resolved repo
   deploy/signing keys with strict pinned GitHub host-key verification, and fail
   rather than falling back to ambient user SSH identities or trust-on-first-use
   host keys.
5. **Agent credentials** — provider keys cross the boundary only through
   declared runtime environment delivery, while Pi auth crosses through the
   platform-specific file delivery described in _Credential Surfaces_. Runtime
   declarations serialize only validated names plus required/optional policy;
   neither channel contributes secret values to Nix evaluation, `ProfileConfig`,
   image config, or image layers.
6. **Audit anchor** — the host establishes one execution record before work and
   atomically adds observed completion, as defined in _Audit Trail_.
   Conversation metadata is optional and execution-specific; unobserved
   completion remains incomplete rather than asserting success or recovery.
7. **Scoped component diagnostics** — a component may emit operator-enabled,
   default-off diagnostics without changing the authoritative audit anchor. Wrix
   does not automatically index, synthesize, or aggregate those artifacts; the
   component owns their contract and the operator owns their destination, access
   control, retention, and deletion.

### Non-Functional

1. **Trust posture** — the launcher's parent process is trusted to choose key
   source paths. Validation is presence-only (`[ -f ]`); no path-prefix,
   ownership, mode, or content check.
2. **Composability** — every wrix launcher behaves identically with respect to
   keys regardless of whether its parent is a shell or another wrix container.
3. **Audit limits** — transcripts are evidence for the policy-leakage threat
   model, not exhaustive records of effects or intermediate tool results.
   Runtimes without transcripts still have execution metadata, not synthesized
   conversation content.

## Out of Scope

- **Key rotation cadence.** Operator responsibility; no spec contract.
- **Additional key-path validation** (ownership checks, path-prefix
  restrictions, mode checks on parent-supplied env paths). The trust-posture
  invariant explicitly forbids these.
- **Global syscall-level or tool-call-level audit synthesis and diagnostic
  aggregation** into Wrix's security audit surface. Explicit, default-off
  component diagnostics are permitted by the scoped exception above but remain
  non-authoritative and separate from the transcript and metadata index.
- **Adversarial-agent threat model** — an agent that deliberately evades its own
  transcript. The audit-anchor invariant does not defend against this.
- **OAuth secrets-file mount** (`/run/secrets/oauth_token`). Env passthrough is
  the chosen mechanism.
- **Image signing** and supply-chain verification of pulled artefacts.
- **Serving the host `/nix/store` to sandboxes** via Harmonia, nix-serve, or
  equivalent host-store-backed binary-cache tools. Wrix's project cache is an
  explicit cache with authorized private/common views, not a host-store view.
  View approval does not itself authorize new population sources.
- **Multi-tenant sharing** of a sandbox between operators.
- **Credential brokering or a host-side agent supervisor** — provider
  credentials retain the documented runtime-delivery model; the entire agent
  remains inside the sandbox.
- **Workload role presets, read-only workspace roles, or validated publishing
  transactions** — Git grants control Wrix-managed key delivery, not arbitrary
  credentials deliberately supplied by the trusted caller, workspace write
  access, or what a granted repository key may do.
- **Protecting Git grant policy from workspace writers** — repository policy is
  trusted input; no independently secured authorization store or agent-resistant
  policy boundary is provided.
- **Automatic recovery, background lifecycle reconciliation, and power-loss
  durability guarantees** — incomplete execution records expose uncertainty;
  they do not restart work or restore transient processes.
