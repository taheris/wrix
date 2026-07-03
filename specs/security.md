# Security

Cross-cutting security invariants for wrix sandboxes: threat model,
credential surfaces, network exfil baseline, and audit anchor.

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
the container/microVM into the host. This is mitigated by the
hardware-virtualized boundary owned by `sandbox.md` (microVM on macOS,
opt-in via `WRIX_MICROVM=1` on Linux).

The normal boundary model excludes host container-runtime control.
`WRIX_UNSAFE_PODMAN_SOCKET` is an operator-declared unsafe exception:
it exposes the host user's Podman API inside the sandbox, which can
start containers with host bind mounts and is therefore outside the
normal sandbox mitigations. It is disabled by default, and the legacy
`WRIX_PODMAN_SOCKET` name has no effect.

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
arbitrary host paths (e.g. `/etc/wrix/keys/`) and passing those
paths through `WRIX_DEPLOY_KEY` / `WRIX_SIGNING_KEY`. Without this
rule the child would boot without keys (the parent's `$HOME` has no
`~/.ssh/deploy_keys/`), agents would produce unsigned commits, and
`git push` would fail.

**In-container destination is fixed.** Regardless of which source
won, the launcher mounts the key at `/etc/wrix/keys/<name>`
(`<name>-signing` for the signing key) inside the container and sets
the child's `WRIX_DEPLOY_KEY` / `WRIX_SIGNING_KEY` env vars to
those in-container paths. The host source path never crosses the
boundary. This makes the launcher recursively composable: every
wrix launch — host-spawned or container-spawned — produces a child
that observes its keys at the same paths under the same env vars.

**Host and repository Git bootstrap.** `cli.md` owns the `wrix init`
command that applies repo-local Git config for host shells, devshells,
containers, and Loom linked worktrees. The security invariant is that
Wrix-managed Git transport uses only the context-resolved deploy key:
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

The agent's API credentials reach the container at **runtime only** — they
are never baked into an image layer (an image is content-addressed and
widely shared, so a secret in a layer leaks). Two delivery channels, both
with mechanics owned by `sandbox.md`:

- **Env passthrough** — claude's `CLAUDE_CODE_OAUTH_TOKEN`, or a non-claude
  agent's provider key (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, …), passed
  through `env` / host env / `SpawnConfig.env`. The exposure surface inside
  the container is `/proc/$pid/environ` of the agent's own process.
- **Credential-file mount** — a file-based agent auth store. For Pi, the
  launcher resolves `WRIX_PI_AUTH_FILE` or falls back to
  `~/.pi/agent/auth.json`. Interactive `wrix run` creates an empty fallback
  file for first `/login` use when it is missing; non-interactive
  `wrix spawn` fails loudly when the file is absent. The launcher mounts only
  that credential path into Pi's `~/.pi/agent/auth.json` when `WRIX_AGENT=pi`.
  Linux uses a single-file bind; macOS mounts the auth file's parent directory
  at an internal staging path because Apple Container/VirtioFS is
  directory-oriented, then exposes only the selected auth file to Pi.

Acceptable because:

- The container runs as a single identity (on the default Linux boundary,
  rootless container-root — kernel uid 0 inside a user namespace owned by the
  unprivileged host user; claude runs with `IS_SANDBOX=1` so it accepts that
  root. The microVM path is equivalent — krun maps the host user to root and
  uses `LD_PRELOAD` libfakeuid). There is no second principal, so the only
  processes that can read `/proc` are ones the agent itself started.
- The credential is already present in the operator's environment (or in an
  on-host auth file); passing it through adds no new host-side exposure.
- Tokens are session-scoped, with limits set by the provider.

A secrets-file mount (`/run/secrets/oauth_token`) would prevent
`/proc/environ` exposure but adds complexity for marginal benefit
against the stated threat model. wrix does not model providers or keys
itself — provider/model defaults and the agent's own credential resolution are
the agent's concern. Pi gets image-baked non-secret `settings.json` defaults and
a runtime `auth.json` mount; its project session persistence uses an explicit
`sessionDir`, not a broad import of `~/.pi/agent`. Claude gets its settings
surface. Wrix only delivers the secret into the container.

### Network Exfil Baseline

Sandbox networking has an always-on local-network isolation baseline owned by
`sandbox.md`: no inbound sandbox ports, IPv6 disabled/blocked for v1, and no
outbound access to LAN/private/host-local/VPN/special ranges except exact
wrix-owned endpoint and DNS exceptions. `WRIX_NETWORK=open` allows public
internet outbound under that baseline. `WRIX_NETWORK=limit` further restricts
public egress to a merged allowlist.

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

Policy-leakage detection is anchored in the selected **agent's own
session transcript**. Claude uses `/workspace/.claude/`, Pi uses
`/workspace/.pi/agent/sessions`, and direct mode uses `/workspace` as
the transcript root for the externally supplied runner. At session end,
wrix writes a **session-metadata index** to
`/workspace/.wrix/log/<timestamp>.json` containing:

- `timestamp_start`, `timestamp_end`, `duration_seconds`
- `exit_code`, `mode`
- `bead_id` (null in non-orchestrated sessions)
- `wrix_session_id`, `claude_session_id`
- `agent_session_dir` — pointer to the selected agent's transcript directory

The index is the **audit anchor**; the agent transcript is the
**audit content**. The index is the smallest artefact that makes a session
findable post-hoc (by bead, by time, by exit code); the agent
transcript is the only place rich enough to reason about what the
agent intended at each step.

Wrix deliberately does not synthesize a parallel
syscall-level / tool-call-level audit log. The agent transcript
already contains intent + reasoning + outcome at the granularity that
matters for policy leakage; OS-level audit (strace, process tree)
would only add value against an adversarial agent that hides its
actions from its own transcript — a different threat class.

### Component-Specific Security (Cross-References)

The following security-relevant facts are owned by sibling specs;
this section is the index, not a restatement.

- **Boundary class** (microVM on macOS always; opt-in via
  `WRIX_MICROVM=1` on Linux; krun memory overhead) — `sandbox.md`
- **Network mode mechanics** (`open` vs `limit`, always-on local-network
  blocking, exact endpoint/DNS exceptions, one-time domain resolution, IPv6
  policy, fail-closed in-sandbox firewall setup, and `capsh` `NET_ADMIN`
  drop) — `sandbox.md`
- **Base allowlist enumeration** — `profiles.md`
- **Per-profile allowlist additions** — `profiles.md`
- **Nix build sandbox disabled inside the container image** —
  `image-builder.md`
- **Builder SSH keys and trust model** — `linux-builder.md`
- **Project Nix cache** (per-workspace explicit binary cache, no host
  `/nix/store` serving, no host Nix daemon socket, no sandbox signing key) —
  `services.md`
- **Host repo Git bootstrap** (`wrix init`, optional `wrix.toml`, helper
  verification, and GitHub deploy/signing key provisioning) — `cli.md`
- **Unsafe host Podman socket opt-in** (Linux-only socket mount mechanics,
  legacy-env rejection, and fail-loud missing-socket behavior) —
  `sandbox.md`

## Success Criteria

- When the launcher's environment sets `WRIX_DEPLOY_KEY` and
  `WRIX_SIGNING_KEY` to existing files outside
  `$HOME/.ssh/deploy_keys/`, the child container observes both env
  vars set to `/etc/wrix/keys/<name>{,-signing}`, the files are
  present at those in-container paths, and `git commit` in the child
  produces a commit whose `git cat-file -p HEAD` output contains a
  non-empty `gpgsig` field.
  [system?](verify:security.nested-key-propagation)
- A fresh spawned sandbox configures global `user.name` / `user.email`,
  installs pinned GitHub host keys at `/etc/ssh/ssh_known_hosts`, uses
  the mounted deploy key with strict host-key checking for GitHub SSH,
  makes an empty signed commit, and verifies that commit as a good SSH
  signature without manual `ssh-keyscan` or `git config`.
  [system?](verify:security.git-ssh-bootstrap)
- Wrix-initialized host Git, container Git, and a `.loom/integration`-style
  linked worktree all use context-resolved repo deploy/signing keys, strict
  pinned GitHub host-key verification, and no ambient user SSH identities;
  a fresh host-side GitHub SSH operation reaches authentication or repository
  authorization without host-key verification failure.
  [system?](verify:security.host-container-loom-git-helper)
- When `WRIX_DEPLOY_KEY` or `WRIX_SIGNING_KEY` is set in the
  launcher's environment but the pointed-at file does not exist, the
  launcher exits non-zero with a stderr message naming the missing
  path, before the container is started.
  [test](crates/wrix-sandbox/tests/launch.rs::missing_key_env_paths_fail_before_container_start)
- Under `wrix spawn`, when a deploy key or signing key does not
  resolve (no env pointer and no `$HOME/.ssh/deploy_keys/` fallback),
  the launcher exits non-zero with a stderr message naming the
  unresolved key, before the container is started; interactive
  `wrix run` still boots without keys under the same condition.
  [test](crates/wrix-sandbox/tests/launch.rs::spawn_requires_resolved_keys_but_run_allows_missing_keys)
- After a sandbox session, a session-metadata index file exists under
  `/workspace/.wrix/log/`; its `timestamp_start`, `timestamp_end`,
  `exit_code`, `mode`, and `agent_session_dir` fields are populated;
  and `agent_session_dir` resolves to an existing directory.
  [system?](verify:security.audit-trail-anchor)
- Default launches do not expose the host Podman API; `WRIX_PODMAN_SOCKET`
  does not enable it, and `WRIX_UNSAFE_PODMAN_SOCKET` is the only opt-in,
  failing loudly when set but the host socket is absent.
  [test](crates/wrix-sandbox/tests/launch.rs::podman_api_socket_requires_explicit_unsafe_opt_in)

## Requirements

### Functional

1. **Host-source resolution precedence** — launcher resolves each
   key's host source by env-first, `$HOME/.ssh/deploy_keys/`-second;
   independently per key; fails loud if env is set but file does not
   exist. Under `wrix spawn`, an unresolved key (no env, no fallback)
   is also fail-loud; interactive `run` permits the no-mount
   fall-through. (See *Credential Surfaces*.)
2. **In-container destination fixed** — `/etc/wrix/keys/<name>` for
   the deploy key, `/etc/wrix/keys/<name>-signing` for the signing
   key; the launcher always sets `WRIX_DEPLOY_KEY` /
   `WRIX_SIGNING_KEY` in the child's env to those in-container
   paths. Host source paths do not cross the boundary.
3. **Platform symmetry** — Linux and macOS launchers implement the
   same precedence rule; behavior is identical across platforms
   modulo the launcher's outer shell/applescript wrapping.
4. **Host/repository Git transport** — Wrix-initialized host Git,
   container Git, and Loom linked worktrees use context-resolved
   repo deploy/signing keys with strict pinned GitHub host-key
   verification, and fail rather than falling back to ambient user
   SSH identities or trust-on-first-use host keys.
5. **Audit anchor** — every sandbox session writes a session-metadata
   index whose `agent_session_dir` field points at the directory
   containing the selected agent's transcript for that session.

### Non-Functional

1. **Trust posture** — the launcher's parent process is trusted to
   choose key source paths. Validation is presence-only (`[ -f ]`);
   no path-prefix, ownership, mode, or content check.
2. **Composability** — every wrix launcher behaves identically with
   respect to keys regardless of whether its parent is a shell or
   another wrix container.
3. **Audit fit** — the agent transcript is treated as fit-for-purpose
   audit content for the stated threat model (policy leakage from a
   misbehaving but not adversarial agent).
4. **Unsafe host-runtime control** — host Podman API access is absent by
   default. When enabled through `WRIX_UNSAFE_PODMAN_SOCKET`, it is outside
   the normal sandbox boundary and inherits the operator's host Podman
   privileges.

## Out of Scope

- **Key rotation cadence.** Operator responsibility; no spec contract.
- **Additional key-path validation** (ownership checks, path-prefix
  restrictions, mode checks on parent-supplied env paths). The
  trust-posture invariant explicitly forbids these.
- **Syscall-level or tool-call-level audit synthesis** by wrix
  itself. The agent transcript is the ground truth; wrix only
  writes the metadata index that makes it findable.
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
