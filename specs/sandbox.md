# Core Sandbox

Platform-agnostic container isolation for coding agents — composes a workspace
profile, an OCI image source, and a launcher binary; runs on Linux (Podman,
optionally krun-backed microVM) and macOS (Apple `container` CLI on
Virtualization.framework).

## Problem Statement

Running AI coding assistants with unrestricted host access creates security
risks. The sandbox must protect host filesystem and processes from container
actions, preserve host UID/GID for workspace files, support outbound network for
research and package management, and work consistently across Linux and macOS
without per-platform consumer code.

## Architecture

`mkSandbox` is the entry point. It composes three concerns owned elsewhere and
returns a sandbox attrset:

- A workspace **profile** — packages, env, mounts, network allowlist, plugins
  (`profiles.md`)
- An OCI **image source** built from the profile and the selected agent runtime
  (`image-builder.md`)
- The independently provided, profile-agnostic host **launcher** (`wrix`;
  installation and root command grammar owned by `cli.md`)

### Sandbox Outputs

Acceptance: [Success Criteria](#success-criteria) and
[Runtime and Configuration Acceptance](#runtime-and-configuration-acceptance).

`mkSandbox` returns `{ image, profile, devShell }`, not a configured CLI:

- `image` — the per-profile OCI image-source derivation/attrset, carrying
  `source`, `ref`, `source_kind`, `digest` and the generated immutable
  `profileConfig`. These paired data feed the shared platform installer and
  explicit host `--profile-config` without re-deriving tags or source kinds.
- `profile` — the resolved profile after consumer package, mount, environment
  and MCP composition.
- `devShell` — the bound host-shell constructor. It automatically supplies this
  sandbox's configuration to the host CLI under
  [Project Launch Handoff](profiles.md#project-launch-handoff).

The host CLI is independently provided under
[Host CLI and Project Configuration](cli.md#host-cli-and-project-configuration).
There are no `package`/`launcher` attributes, configured `wrix`/ `wrix-run`
executables or runnable `sandbox-*` compatibility targets. Programmatic
consumers use `image.profileConfig` or the data-only
[Profile Manifest](profiles.md#profile-image-manifest) with host `wrix`.

### Platform Dispatch

`lib/sandbox/default.nix` selects the platform image source, entrypoint, and
generated launch metadata, then rejects unsupported systems at evaluation. The
profile-agnostic Rust `wrix` launcher performs host runtime dispatch at
execution time: Linux constructs the Podman invocation, and Darwin constructs
the Apple `container` invocation.

The **runtime image installer** is the shared host-side install path used by
`wrix run`, `wrix spawn` and `wrix service start`; it is not a separate public
CLI. Broad image pruning is separate from installation and follows
[Image Retention and Cleanup](#image-retention-and-cleanup).

### Image Install Path

Before invoking the platform install pipeline, the wrix runtime image installer
checks whether the image's **content digest** recorded with the selected image
source (not ref-name+tag) matches any image already present in the platform
store. On Linux this digest is derived from descriptor/config metadata without
executing the source; tar-loadable Darwin sources may be inspected for config
metadata but are not loaded. On a digest hit, the install is skipped entirely —
no source execution, no tar materialization, no stream invocation, and no
`*-load` CLI call. On a miss, the installer dispatches by
`ProfileConfig.image.source_kind` according to the stable source-kind contract
owned by `image-builder.md`:

- The Linux descriptor source names a prebuilt OCI layout. The runtime installer
  reads that descriptor and copies `oci:<oci_layout>:<oci_ref>` into
  `containers-storage:<ref>` with skopeo (or an equivalent wrix-owned copy
  path). The digest-presence check runs before the copy; on a miss, wrix
  delegates the copy to the destination store's content-addressed transport.
- The Darwin tar-loadable source is converted to a temporary OCI archive before
  `container image load --input <oci-archive>`. Apple's `container` CLI surfaces
  no per-blob-dedup install path at this time; see `image-builder.md` § Out of
  Scope.

Both platforms rely on the provenance-tiered graph (see `image-builder.md` §
Provenance-Tiered Layering) to keep volatile changes isolated. Linux realizes
the cache contract through descriptor-level layer reuse; Darwin keeps a tar/load
fallback plus the digest-presence check until a per-blob Apple path is verified.
This installer check establishes image presence, not launch readiness; read-only
[Launch Preflight](#read-only-launch-preflight) is the separate preview
contract.

### Image Retention and Cleanup

Acceptance: [Success Criteria](#success-criteria).

Wrix-initiated broad image pruning is an explicit maintenance operation through
`wrix service gc image`, not part of `run`, `spawn` or service startup. Normal
operations still use the digest-presence check, reuse matching images and
install missing images, but do not evict other cached images. There is no
automatic MRU-eight pruning or automatic disk cap imposed by those operations.
Cached images may accumulate until explicit cleanup; a launch in one repository
does not silently prune another repository's warm image.

Bare `wrix service image` provides read-only runtime-image information, without
installation, pruning or usage/retention bookkeeping writes. CLI grammar belongs
to [Image Commands](cli.md#image-commands); this spec owns the shared runtime
mechanics. Image outputs retain the ownership metadata from `image-builder.md`;
moving the pruning trigger does not relabel third-party images. Combined
`wrix service gc all` includes this same collector alongside host-user-wide
Dolt/Celler collection; plain workspace/host data GC does not prune images. The
`all` selector does not relax image ownership or usage protections. Composition
and argument admission belong to [CLI Service Scope](cli.md#service-scope) and
[Service GC Scope](services.md#garbage-collection-scope).

Temporary import-reference cleanup is distinct from broad pruning. On Darwin a
successful archive load is first tagged with its stable Wrix ref, then its
operation-created `untagged@sha256:<digest>` load ref is removed. That
housekeeping must not evict unrelated cached images or remove the installed
image.

Runtime-unused means no existing container, running or stopped, references or
depends on the image and no Wrix installation/launch reserves it before
container creation. Usage checks resolve actual image IDs/digests and runtime
references; tags such as `latest` or `<none>`, age and configured-profile
selection alone do not determine usage. A current-but-idle cached image can
therefore be unused and require another import after collection. Unused does not
itself establish image ownership or deletion eligibility.

Explicit GC targets all runtime-unused images positively identified as
Wrix-managed by the
[image ownership metadata](image-builder.md#image-ownership-labels), across
profile, service and support kinds. Recency, current-profile selection and the
former MRU-eight list do not pin an otherwise eligible image. Unlabelled,
ambiguous and third-party images remain untouched: a Wrix-looking name or
dangling tag alone is not ownership proof. Collection does not relabel images to
make them eligible. This is the host user's shared runtime-image store, not a
per-workspace image pool; selecting a repository does not authorize a different
image set.

Explicit collection preserves container/in-flight references and does not
force-remove containers. If usage cannot be established reliably, the image is
not deleted. Runtime-managed dependencies are not bypassed by manually deleting
layer files or forcing removal. The previous MRU-eight deletion policy is not a
required policy for the explicit collector.

Image GC continues other independently safe image units after an inspection or
removal failure, without deleting a candidate whose safety cannot be
established. Failures and any completed removals are accumulated for the final
report/nonzero outcome under
[GC Failure Handling](services.md#gc-failure-handling); successful removals are
not rolled back.

### Boundary Class

- macOS: microVM via Virtualization.framework, always
- Linux: rootless container by default; `WRIX_MICROVM=1` opts into
  `podman --runtime krun` when `/dev/kvm` is available. krun is bundled via Nix;
  without KVM the opt-in fails loudly rather than silently degrading.

The host Podman API is outside the normal sandbox boundary. Linux exposes it
only through the explicit unsafe operator opt-in `WRIX_UNSAFE_PODMAN_SOCKET`,
which mounts the host user's Podman socket and exports `CONTAINER_HOST` for
sibling-container workflows. The legacy `WRIX_PODMAN_SOCKET` name has no effect.

Threat-model rationale for these choices lives in `specs/security.md`.

### Network Posture

Acceptance: [Network Posture Acceptance](#network-posture-acceptance) and
[Network and Mount Enforcement Acceptance](#network-and-mount-enforcement-acceptance).

`WRIX_NETWORK` selects public egress posture. Both platforms install an
in-sandbox firewall before workspace setup or agent execution, using immutable,
image-pinned startup tools. Linux Podman uses nftables by default; Darwin uses
the available verified guest backend, not host `pf`.

Both modes block inbound ports, disable/block IPv6 for v1, and protect
LAN/private/host-local/VPN/special IPv4 destinations. Only designated Wrix
data-plane endpoints receive exact address/protocol/port exceptions. Host
management endpoints are not Wrix data-plane exceptions.

- `open` remains the default: public-internet outbound is allowed, with the
  protected-address baseline and exact configured TCP/UDP port 53 resolver
  exceptions.
- `limit` requires the authenticated proxy owned by `proxy.md`. The agent
  firewall permits only the designated proxy TCP endpoint and required Wrix data
  endpoints, not direct public destinations or public DNS. The proxy owns
  hostname/port admission and resolution using the structured rules owned by
  [Profiles Network Allowlist](profiles.md#network-allowlist). There is no third
  mode or direct-IP allowlist fallback. Unknown mode values fail before startup.

The proxy retains the same protected-address baseline for its connections, with
only exact required resolver exceptions. These exceptions do not grant agents
unrestricted DNS access. Resolution failure affects the requested proxy
connection, not launch merely because an unused allowed hostname is unavailable.

Filtering is fail-closed. Temporary in-container `NET_ADMIN` is granted only for
trusted startup. The bootstrap verifies policy, then replaces itself through
`capsh` with a stage rejecting `NET_ADMIN` in every capability set before
workspace setup, agent execution or exit logging. Rootless Podman, optional krun
and Apple microVMs share that sequence. Failure to verify firewall setup, IPv6
blocking or capability drop prevents launch. Missing/unavailable proxy
infrastructure prevents restricted launch; subsequent failure never enables
direct egress. Host firewall mutation and LAN-open fallback are excluded.

### Network and Mount Enforcement Verification

Acceptance:
[Network and Mount Enforcement Acceptance](#network-and-mount-enforcement-acceptance).

Live acceptance exercises the production launcher and workload under each
supported isolation boundary: Linux rootless Podman, Linux krun with usable KVM,
and Darwin Apple Container. Shared bootstrap tests supplement rather than
replace these runtime-specific observations. Each boundary reports its own
results and prerequisites using `cli.md`'s verifier capability contract;
unavailable platforms or capabilities do not become passing coverage.

Network controls demonstrate changes in enforcement, not just generated rules.
The controls compare `open` and `limit` against known-live public and local
destinations, preserving the existing baseline owned by _Network Posture_.
Open-mode DNS, restricted-mode proxy and Wrix-managed TCP endpoint exceptions
allow only the selected address, protocol and port. Restricted-mode controls
also exercise authorized proxy requests versus direct and public-DNS bypass.
Controls exercise the permitted endpoint, a known-live wrong-address endpoint, a
known-live wrong-port endpoint, and the original endpoint with its exception
removed. Independent positive controls establish endpoint health and
comparable-path reachability; a dead service, unresolvable name, or unrelated
routing failure cannot count as a successful policy denial.

Mount controls exercise both profile and `SpawnConfig` declarations for regular
files and directories. Workload writes fail for read-only declarations and
succeed for corresponding writable declarations; read-only host sources remain
unchanged. These controls do not strengthen Darwin's documented directory
persistence or synchronization semantics.

Built-in language cache preparation follows
[Tool Cache Data Boundary](profiles.md#tool-cache-data-boundary), rather than
mounting/copying personal host Cargo/sccache/uv caches. That owner's live
acceptance covers host-separated tool use and its
[Shared/Private Modes](profiles.md#shared-and-private-tool-cache-modes). Shared
mode enables the host-user worker pool across repositories; sharing-off uses
fresh private writable cache data for the whole launch, not a subset of
sources/directories. These managed-cache requirements do not change ordinary
directory staging/synchronization or restore guest Nix persistence. Read-only
preflight does not allocate/populate either shared or private cache data. Run
and spawn use the same explicit-choice, selected-workspace repository,
shared-default resolver from that owner; explicit choices work in both
directions without rewriting policy. Its
[Shared Compiler](profiles.md#shared-rust-compiler-service) contract owns
central sccache execution and requires private caches for restricted Rust;
central compiler work is not automatically covered by worker resource limits.
Read-only inspection reports the resolved choice under the same launch policy
instead of silently replacing a private repository default.

This acceptance scope covers networking and mounts, not a new resource-limit
policy or workload-level CPU, memory or process-limit assurance. Existing
resource controls remain delegated to the selected container runtime.

### Agent Runtime Axis

The `agent` parameter selects, **at build time**, the agent runtime whose
executable the entrypoint launches. That executable must be in the image, so
this is not a runtime knob.

- `pi` (default) — the packaged Pi coding agent; no consumer package required
- `claude` — `claude-code` from nixpkgs; no consumer package required
- `direct` — a consumer-supplied runner, requiring `agentPkg` with an explicit,
  nonempty, single-component `meta.mainProgram`

For `direct`, the entrypoint executes
`${agentPkg}/bin/${agentPkg.meta.mainProgram}` from the selected image, with
agent arguments and stdio preserved. Missing package or executable declaration
fails Nix evaluation; a missing executable fails before agent execution. There
is no placeholder runner or Wrix-mandated runner name. Wrix does not interpret
the consumer runner's stdio protocol.

The selector's closed set and runtime meanings are owned here. Physical image
composition and the scope of automatic runtime exclusivity are owned by
`image-builder.md` § Provenance-Tiered Layering. The agent tier composes
orthogonally with the profile, so variants are `(profile × agent)`.

#### Agent Selection

Selection is by build target, not by caller env. `WRIX_AGENT` is the internal
wire the entrypoint reads, but callers do not select it by exporting env vars. A
human selects an agent in `mkSandbox { agent = …; }`; the paired image and
immutable `ProfileConfig` encode that choice. Orchestrators use the host CLI
with the matching explicit per-call configuration, not a project-selected raw
launcher executable.

#### Entrypoint Agent Guards

The image declares its baked agent variant in `/etc/wrix/image-agent`. The
entrypoint dispatches on `WRIX_AGENT` and, before exec, first rejects a mismatch
between the ProfileConfig-selected agent and the image-declared agent with a
clear ProfileConfig/image-variant error, then verifies the named binary is
present (`command -v`). A request for an agent absent from the image — e.g.
`WRIX_AGENT=pi` against a claude image through explicit profile configuration —
fails loudly with a clear error instead of a bare `command not found`.

#### Per-Agent Configuration

Each agent keeps its own config system; Wrix delivers configuration rather than
abstracting it:

- _Config home_ — the entrypoint seeds the selected agent's config home from
  baked defaults: claude → `~/.claude`, pi → `~/.pi/agent`; `direct` has none.
  Session data persists in the documented workspace locations.
- _Credentials_ — API keys and OAuth/subscription tokens reach the agent through
  declared `runtimeSecrets`, same-named host env or `SpawnConfig.env`, and
  credential-file mounts. Static profile/mkSandbox env is non-secret and
  image-baked. The credential invariants are owned by `security.md`.
- _Package/settings overrides_ — `agentPkg` overrides the selected agent
  package; `agentSettings` merges into the selected agent's settings schema. For
  `agent = "direct"`, omitted or empty `agentSettings` is accepted; nonempty
  settings are rejected because direct has no Wrix-owned settings schema.

### Pi Settings

Wrix seeds native Pi settings and honors consumer `agentSettings` overrides.
Additive codemode is enabled by default with `defaultTools = [ "+codemode" ]`
and `codemode.mode = "on"`, including when no MCP servers are selected. Read,
Bash, edit, and write remain directly available. Consumers can replace these
defaults through Pi's own settings; Wrix adds no codemode interpreter or
tool-orchestration abstraction. Model selection, reasoning level, and cosmetic
defaults are configuration choices, not fixed sandbox contracts. Pi defaults to
`defaultProjectTrust = "always"` inside the container and
`enableInstallTelemetry = false`; neither setting replaces the container
security boundary. Session persistence uses an explicit
`/workspace/.pi/agent/sessions` directory rather than importing arbitrary
workspace agent-home files.

### MCP Servers

`mkSandbox`'s `mcp` parameter opts registered servers in per sandbox, for
example `mcp.playwright = { … }`; `playwright-mcp.md` owns that server's
contract. Native terminal debugging is owned by `tmux.md`, not an MCP server.
Wrix normalizes selected stdio servers into a schema-v1 manifest whose entries
carry `name`, `command`, `args`, and `env`, exporting its path as
`WRIX_MCP_MANIFEST` for every agent. Explicit `mcp` selects its declared
servers. `mcpRuntime = true` bakes every registered server and selects them at
launch through `WRIX_MCP`: unset or `all` selects all, an empty value selects
none, and a comma-separated list selects named servers. Unknown names fail
startup.

Wrix translates the selected manifest into native client configuration: Claude's
`mcpServers` and Pi's container-local `~/.pi/agent/mcp.json`. Generated Pi
configuration contains only the current selection and is regenerated on launch,
never copied back into host configuration. Trusted project `.pi/mcp.json`
follows Pi's native precedence, including same-name overrides; `WRIX_MCP`
controls Wrix-managed selection, not arbitrary project capabilities.

Pi owns MCP transport, discovery, namespacing, cancellation, shutdown, and
result presentation. Wrix exposes Pi-native names such as
`mcp__playwright__browser_snapshot`, without unqualified aliases or a custom
protocol client. Wrix-managed Pi servers use native `codemode` exposure;
codemode receives complete MCP results, including `structuredContent`,
`content`, and `isError`, while direct presentation follows Pi's own rules.
Direct runners receive the unchanged manifest handoff and own its consumption.
Profile output naming remains in `profiles.md`.

## mkSandbox API

```nix
mkSandbox {
  profile = profiles.base;          # Workspace profile (profiles.md). Default: base
  cpus = null;                      # CPU limit honored by the platform launcher
  memoryMb = 4096;                  # Memory limit MB
  deployKey = "myproject";          # Default key identity, not permission to mount keys
  packages = [ pkgs.jq ];           # Extra packages merged into profile.packages
  mounts = [ {                      # Extra mounts merged into profile.mounts
    source = "~/.config";
    dest = "/home/wrix/.config";
    mode = "ro";                    # "ro" or "rw"
  } ];
  env = { FOO = "bar"; };           # Non-secret env merged into profile.env
  runtimeSecrets = {                 # Runtime values resolved by the host launcher
    OPENAI_API_KEY = "required";     # "required" or "optional"
  };
  mcp.playwright = { };             # MCP server opt-in
  mcpRuntime = false;               # Bake ALL MCP servers, defer selection to entrypoint
  agent = "pi";                     # "pi" (default), "claude", or "direct"
  agentPkg = null;                  # Required with meta.mainProgram for direct
  agentSettings = { };              # Settings for the selected agent
}
```

Returns `{ image, profile, devShell }`; generated launch configuration is
available as `image.profileConfig`.

## Launcher Runtime Contract

`cli.md` owns that the launcher is exposed as `wrix run` and `wrix spawn`,
including help and root dispatch. This section owns the runtime contract behind
those subcommands. The Rust `wrix` launcher binary is profile-agnostic. Nix
supplies build-time defaults through an immutable `ProfileConfig` JSON file,
selected under [Project Configuration Handoff](#project-configuration-handoff)
by explicit `--profile-config <path>` or the generated devshell default. Both
launcher subcommands share container construction (mounts, env passthrough,
runtime selection, deploy key, workspace service startup, network firewall
configuration); they differ only in stdio and per-launch configuration source.

Git grants are resolved independently for `deploy` and `sign`: explicit launch
override, then `[git]` in the selected workspace repository's root `wrix.toml`,
then `false`. Outside a repository there is no repository policy tier.
Repository policy is read anew for each launch. The trusted caller can grant or
remove either capability. `cli.md` owns repository policy names; `security.md`
owns the repository-policy trust boundary, key resolution, and credential
exposure. Profiles select key identity, not grants.

`wrix run` accepts `--git-deploy` / `--no-git-deploy` and `--git-sign` /
`--no-git-sign` as invocation-only launcher options. Supplying both sides of a
pair in the launcher-option prefix is an error. Parsing launcher options stops
at the first positional argument or `--`, whichever comes first. A positional
argument before `--` selects the workspace; without one, the workspace is CWD.
After the workspace, one optional leading `--` is consumed and all remaining
arguments are passed unchanged to the agent, even if they match Wrix option
names. With no workspace argument, `--` is required to pass agent arguments.

With selected launch configuration, `wrix run --git-sign /repo -- --help` grants
signing and passes `--help` to the agent in `/repo`.
`wrix run --no-git-sign -- --help` uses CWD and disables signing. `AGENT_ARGS`
are the selected agent's argv, not a Wrix-interpreted shell command.

`wrix spawn` uses optional `SpawnConfig.git.deploy` and `SpawnConfig.git.sign`
booleans; omitted fields inherit independently. Invalid policy or override types
fail before service startup or credential staging. `WRIX_GIT_SIGN` is not a
policy input.

Before launching the agent container, both subcommands ensure the shared
host-user service and selected workspace namespace when Beads/cache are enabled,
or the proxy when restricted networking requires it. `services.md` owns the host
controller, lifecycle and shared-role endpoints with scoped native
authorization; `cache.md` owns one-way host publication, sandbox cache
transport/trust and
[client publication freshness](cache.md#client-publication-freshness). This spec
owns their launch-time injection for both modes, including the cache-enabled Nix
client context. `proxy.md` owns launch traffic grants. Both subcommands use the
same container construction path. The launcher admits host-controller
compatibility before preparation and never supplies its control socket, issuer
authority or runtime-management access to the guest. New namespaces use existing
role listeners; neither native integration gaps nor a shared endpoint justify
weaker firewall exceptions or dynamic serving-mount changes.

| Subcommand                                           | Stdio                         | Configuration source                                 | Use case                                           |
| ---------------------------------------------------- | ----------------------------- | ---------------------------------------------------- | -------------------------------------------------- |
| `wrix run [LAUNCH_OPTIONS] [DIR] [--] [AGENT_ARGS…]` | TTY (`-it`)                   | `ProfileConfig` JSON + host env + CLI args           | Interactive sessions through the host CLI          |
| `wrix spawn --spawn-config <file> [--stdio]`         | Non-TTY; stdin with `--stdio` | `ProfileConfig` JSON + per-launch `SpawnConfig` JSON | Programmatic dispatch (loom; future orchestrators) |

`wrix spawn` stays in the foreground until the container command finishes and
returns its observed exit status. It does not allocate a TTY. `--stdio` enables
container stdin and requests the selected agent's stdio mode; it does not
detach. Callers may background the launcher themselves, but Wrix does not take
over its supervision. Execution-record completion follows `security.md`. For
cache-enabled run/spawn, that same waiting launcher owns automatic
[multi-day credential renewal](cache.md#launcher-owned-renewal) and a private,
read-only-mounted native Nix credential directory. Runtime/agent completion
stops renewal; no separate detached owner is introduced. Renewal failure alone
is not execution completion or permission to alter observed agent exit status.

`ProfileConfig` JSON is generated by Nix into the store and contains the
immutable profile/image defaults. It is data, not shell code, and the Rust CLI
validates it before constructing platform container argv. `ProfileConfig`
remains schema 1, including the strict owned-field and structured network-rule
contract. Producers and the parser adopt that shape directly; no legacy-shape
reader or compatibility migration is provided. Unsupported schema versions are
rejected. Acceptance:
[Runtime and Configuration Acceptance](#runtime-and-configuration-acceptance)
and [Strict Configuration Acceptance](#strict-configuration-acceptance).

Schema v1:

```json
{
  "schema": 1,
  "system": "x86_64-linux",
  "profile": {
    "name": "example",
    "env": { "KEY": "value" },
    "mounts": [
      { "source": "~/.config/example", "dest": "/home/wrix/.config/example", "mode": "ro", "optional": false }
    ],
    "writable_dirs": [
      "/home/wrix/.cargo",
      "/home/wrix/.cache"
    ],
    "network_allowlist": [{ "host": "example.org", "ports": [443] }]
  },
  "image": {
    "ref": "localhost/wrix-example:sha256-...",
    "source": "/nix/store/...-wrix-example-image.json",
    "source_kind": "<platform-source-kind>",
    "digest": "sha256:..."
  },
  "agent": {
    "kind": "pi"
  },
  "resources": {
    "cpus": null,
    "memory_mb": 4096,
    "pids_limit": 4096
  },
  "security": {
    "deploy_key": null,
    "runtime_secrets": {
      "ANTHROPIC_API_KEY": "optional",
      "CLAUDE_CODE_OAUTH_TOKEN": "optional",
      "OPENAI_API_KEY": "optional"
    }
  },
  "network": {
    "default_mode": "open",
    "ipv6": "disabled"
  },
  "services": {
    "beads": { "enable": "auto" },
    "nix_cache": { "enable": true }
  },
  "features": {
    "mcp_runtime": false
  }
}
```

`profile.mounts` are profile-level mounts and are additive with
`mkSandbox.mounts` and `SpawnConfig.mounts`. `profile.env` is non-secret
profile/default container environment; it enters the Nix store and OCI image
metadata and is overridden only by explicit per-launch env rules.
`image.source_kind` uses the stable values and meanings defined by
`image-builder.md`; this spec owns validation and install dispatch, not those
values. `security.runtime_secrets` maps validated environment names to
`optional` or `required` and contains no values; the launcher resolves values
from `SpawnConfig.env` or same-named host env immediately before launch.
`agent.kind` is one of `direct`, `claude`, or `pi`; callers may not change it
independently of `image`. `services.beads.enable = "auto"` means start Dolt when
the workspace has Beads configuration and registered managed state as defined by
`beads.md`, including its safe migration contract; `services.nix_cache.enable`
controls project-cache endpoint injection. `network.default_mode` defaults to
`open` and may be overridden at launch by `WRIX_NETWORK=open|limit`; both modes
keep the local-network isolation baseline. `profile.network_allowlist` preserves
the structured rules owned by
[Profiles Network Allowlist](profiles.md#network-allowlist); no hostname-only or
implicit-port form is accepted.

`SpawnConfig` JSON has stable top-level fields:

- `image_ref` — optional per-launch image ref override; when absent,
  `ProfileConfig.image.ref` is used.
- `image_source` — optional per-launch Nix store path of the image source; when
  absent, `ProfileConfig.image.source` is used.
- `image_source_kind` — optional per-launch source-kind override under the
  `image-builder.md` source-kind contract; when absent,
  `ProfileConfig.image.source_kind` is used. If `image_source` is present,
  `image_source_kind` must also be present, even when it matches the profile
  kind, so source overrides never rely on launcher inference. For an
  `image_source` override, the installer derives and validates the selected
  image digest from that override source instead of reusing
  `ProfileConfig.image.digest`. The selected digest informs both read-only
  launch preview and the installer's digest-presence check. Only actual launch
  installs a missing image before container creation; installation dispatch is
  documented in [Image Install Path](#image-install-path).
- `workspace` — host path bind-mounted at `/workspace`
- `env` — per-launch `[key, value]` pairs to pass through; pairs matching
  `security.runtime_secrets` are runtime credential sources and never enter Nix
  or image data
- `agent_args` — argv tail passed to the agent binary. Claude stream-json
  clients can explicitly opt in to its permission control protocol with
  `["--permission-prompt-tool", "stdio"]`; Wrix does not add this flag by
  default. This leaves the existing `--dangerously-skip-permissions` behavior
  unchanged, not an approval enforcement boundary. See
  [Claude stdio clients](../docs/claude-stdio.md) for client/version
  compatibility. [system](bash tests/sandbox/entrypoint-contract.sh
  test_claude_stdio_permission_prompt_opt_in_both_entrypoints)
- `git` — optional object with independently optional boolean `deploy` and
  `sign` overrides; absence inherits repository policy, explicit `false`
  disables the corresponding grant, and explicit `true` enables it
- `tool_caches` — optional owned object with optional boolean `shared`; omission
  inherits, `false` selects private launch-local caches and `true` selects the
  shared worker pool under [Tool Cache Selection](#launch-tool-cache-selection)
- `mounts` — optional `[{host_path, container_path, read_only}]` list; omitted
  or empty means no per-launch mounts. Additive to `profile.mounts` and
  `mkSandbox.mounts`.

- `extensions` — optional object carrying opaque consumer-owned payloads;
  consumer fields are not permitted alongside Wrix fields at the root.

The original config remains mounted read-only inside the container at the path
named by `WRIX_SPAWN_CONFIG`. The strict ownership and extension rules are
specified in
[Strict Configuration Boundaries](#strict-configuration-boundaries). The schema
is part of the launcher runtime contract, and CLI help mirrors it per `cli.md`.
Per-launch `SpawnConfig` may override launch-time inputs (workspace, env
allowlist, agent args, Git grants, tool-cache sharing, mounts, image ref/source
for orchestrators), but it may not change the selected agent independently of
the image/profile config. `wrix run` errors when no valid `ProfileConfig` is
supplied; there is no implicit default image baked in.

### Trusted execution tickets

Host callers may reserve a one-shot execution ticket with
`wrix execution reserve <workspace>` and pass it to Linux spawn using
`--execution-ticket <ticket>` or host launcher environment
`WRIX_EXECUTION_TICKET` (an explicit argument takes precedence). Neither is read
from worker environment entries in SpawnConfig. Linux profiles advertise typed
`features.execution_tickets`; this compatibility hint is not lifetime authority.
The ticket and runtime binding live only in the host-private control database,
not SpawnConfig or the workspace audit index. Podman creates the container
without starting it; Wrix durably binds its full immutable ID before
attaching/starting that exact object. Existing resource, network, mount and
credential construction is unchanged. Ticketed launches refuse the unsafe host
Podman socket and are unavailable on Darwin.

`wrix execution projection <workspace> <ticket>` reads only the exact bound
object's ID and mount source/destination/writable fields. It never returns
runtime environments, discovers arbitrary containers, changes state or supplies
retirement authority. This host-only observation supports substrate conformance
without giving consumers direct runtime access.

`wrix execution reconcile <workspace> <ticket>` reports protected or retired.
Only one exact bound object in an unambiguously exited state may be removed,
without force. Retirement requires successful removal returning that exact ID
and a durable host state transition. Missing objects, unavailable/malformed
runtime observations, active/created/paused/restarting objects, conflicting IDs,
setup failures and interrupted removal retain protection. A crash after removal
but before recording retirement remains unknown. Launcher completion, free
leases, PID absence/reuse, worker-written completion and age never authorize
retirement. Tickets cannot be reused. This is explicit host reconciliation, not
a background supervisor or source/output collector. It does not cover delegated
service-side compiler lifetimes or trusted-native processes.

Acceptance:
[test](../crates/wrix-sandbox/tests/config_contract.rs::execution_capability_is_typed_profile_metadata_not_worker_authority)
[test](../crates/wrix-sandbox/tests/execution_lifetime.rs::execution_restart_retains_unbound_launch_and_ignores_workspace_completion)
[test](../crates/wrix-sandbox/tests/execution_lifetime.rs::execution_unavailable_ambiguous_and_failed_removal_observations_retain_binding)
[system](cargo test -p wrix-sandbox --test execution_lifetime
execution_actual_podman_survives_launcher_death_and_reconciles_exact_teardown --
--ignored --nocapture)

### Project Configuration Handoff

Acceptance:
[Project Configuration Acceptance](#project-configuration-acceptance).

The host CLI consumes immutable data generated by `mkSandbox`, without a project
binary. [CLI Configuration Selection](cli.md#host-cli-and-project-configuration)
supplies the selected explicit or generated-default file; the automatic default
is delivered by [Project Launch Handoff](profiles.md#project-launch-handoff).
Explicit input does not require a devshell. Missing or invalid selected data
fails actionably, without a built-in image, repository search/evaluation or
alternate executable fallback.

The same resolver and strict schema-1 parser serve inspection and actual launch.
Malformed data or unsupported declared compatibility fails before service
startup, staging or runtime dispatch. The Nix constructor preserves its
image/configuration pairing. Independent host/project pins are validated, not
assumed compatible or silently redirected to another CLI. This admission is not
proof of all image contents or future runtime behavior; actual launch retains
current-state and image-entrypoint guards. The project pointer supplies launch
data only, not host service-image, management or installation authority.

Per-project configured launchers, `mkSandbox.package`, `mkSandbox.launcher`,
runnable sandbox targets and manifest-provided executables have no compatibility
path. Pure image/configuration outputs, explicit `--profile-config` and
per-launch `SpawnConfig` remain supported. CLI-level selection/help is owned by
[Host Configuration](cli.md#host-cli-and-project-configuration).

### Launch Nix Store Selection

[Service-Owned Nix Stores](store.md#selection-and-host-approval) owns the
independent private/shared selection, host approval and restricted-mode
rejection for run/spawn. Its
[Bootstrap and Lifecycle](store.md#bootstrap-protection-and-lifecycle) contract
owns trusted store setup and lifetime protection. Caller mounts or raw Nix
environment cannot substitute for that admitted path. Acceptance:
[Store Selection](store.md#selection-acceptance) and
[Store Platforms](store.md#platform-acceptance).

### Launch Tool Cache Selection

Acceptance: [Tool Cache Launch Acceptance](#tool-cache-launch-acceptance).

`wrix run` accepts `--cache` (shared) and `--no-cache` (private) as
invocation-only launcher options under the existing prefix rule. Supplying both
before the argument boundary is an error before preparation. After the workspace
or forwarding `--` boundary, matching tokens remain opaque agent arguments and
do not change Wrix policy. There are no long-name compatibility aliases.

`wrix spawn` takes the explicit choice from optional
`SpawnConfig.tool_caches.shared`. The object/key may be omitted to inherit;
explicit `false` selects private mode and explicit `true` selects shared mode.
Only the owned optional boolean is supported, not path/source exclusion maps.
Malformed policy/override shapes and unknown owned fields fail before service
startup, cache allocation, credential staging or container work, even if another
input tier would otherwise override them.

Both modes use [Repository Tool Cache Policy](cli.md#tool-cache-controls) and
the [Profiles Resolver](profiles.md#shared-and-private-tool-cache-modes):
explicit launch choice, then selected-workspace repo policy read anew, then
shared default. Choices do not persist or rewrite repository files. Preflight
reports the effective shared/private choice without preparing either cache mode.
`--no-cache` does not disable tool operation or Nix/Celler substitution, change
`[cache].enable`, or introduce a mode environment variable.

[Shared Compiler Selection](profiles.md#shared-rust-compiler-service) delegates
shared open-mode Rust compilation to the service daemon. Restricted Rust must
resolve to private mode (`--no-cache` or equivalent inherited/spawn choice).
Shared-plus-limit is a configuration error, not silent coercion, before service
startup, cache/input preparation, image work or credential/grant staging.
Preflight retains the resolvable plan, reports an actionable blocker and exits
nonzero without mutation. Restricted/private launches receive no shared compiler
execution route; ordinary Nix-cache/Dolt exceptions do not grant one.

### Strict Configuration Boundaries

Acceptance: [Strict Configuration Acceptance](#strict-configuration-acceptance).

Both `ProfileConfig` and `SpawnConfig` reject unknown fields at their roots and
throughout Wrix-owned nested objects before launch preparation. Supported fields
are explicitly recognized and type-checked, including Nix-generated `system`,
`features`, and service declarations; a field does not become an extension
merely because a particular launcher does not use it. Existing defaults,
omission rules, image-source semantics and duplicate-field rejection remain part
of the parser contract.

Each config has one optional top-level `extensions` object for opaque
consumer-owned data. Omission means no extension payload; a present value must
be a JSON object. Its contents are not Wrix launch instructions and cannot
override agent selection, image, mounts, grants, tool-cache sharing, networking
or resources. Consumer fields at the root or within Wrix-owned objects are
errors rather than compatibility aliases. `SpawnConfig`'s original file,
including `extensions`, remains mounted read-only at the path named by
`WRIX_SPAWN_CONFIG` for the consumer entrypoint.

For example, consumer prompt and repinning data use this shape:

```json
{
  "workspace": "/repo",
  "env": [],
  "agent_args": [],
  "mounts": [],
  "extensions": {
    "initial_prompt": "consumer field",
    "repin": true
  }
}
```

Unknown-field rejection applies to structural fields, not open data-map keys.
Environment and runtime-secret maps still accept arbitrary names satisfying
their existing validation and policy rules. Native `agentSettings` remain owned
by the selected agent, as described in _Per-Agent Configuration_, rather than
becoming a Wrix-owned settings schema.

Rust parsing is authoritative. Concise field references and examples document
the wire contract, and acceptance feeds real Nix-generated configuration through
the Rust parser. Standalone formal schema artifacts and fuzzing infrastructure
are not part of this contract; focused malformed-input and duplicate-field
regressions remain required.

### Read-only Launch Preflight

Acceptance: [Launch Preflight Acceptance](#launch-preflight-acceptance).

`wrix run --dry-run` and `wrix spawn --dry-run` inspect a prospective launch
without preparing or executing it. This is the sole dry-run interface.
`WRIX_DRY_RUN`, `WRIX_DRY_RUN_SERVICES`, and `WRIX_DRY_RUN_SERVICE_BIN` have no
effect; there are no aliases, deprecation errors, or compatibility guards.

For `run`, `--dry-run` is a launcher option only before the workspace or `--`;
an occurrence in the forwarded agent arguments remains an agent argument. For
`spawn`, it composes with `--spawn-config` and `--stdio`. The same configuration
parsing, repository policy, and precedence rules as a real launch determine the
prospective plan.

Output is human-readable by default. With `--dry-run`, `--json` selects one
schema-versioned JSON report of the same plan and findings, with stable finding
codes for machine consumers. It is not an agent stdio mode. Without `--dry-run`,
`--json` in the launcher-option position is an error; both flags obey `run`'s
existing agent-argument boundary. JSON stdout contains only the report,
including on readiness failure; incidental probe diagnostics cannot corrupt it.
Invalid-input reports identify failure without inventing a resolved plan.

Both renderings are purpose-built summaries, not configuration or command dumps.
They omit all environment values, agent argument contents, opaque
consumer-defined `SpawnConfig` payloads, and credential-file contents. Findings
and inspection diagnostics obey the same disclosure boundary; raw input or
probe-output fragments are not an escape hatch.

Inspection reports the selected profile, agent, image, runtime and isolation
boundary; workspace and additional mount intents; network mode and allowlist;
CPU, memory and process limits; independent Git grants and their policy
provenance; and credential names, source selection and availability without
credential contents. Unallocated service endpoints and preparation-only mount
paths remain explicitly unresolved rather than fabricated.

Dry-run performs bounded, non-mutating local readiness checks for the selected
runtime, requested KVM access, required mount sources, and required credential
sources. KVM readiness requires access to the virtualization interface, not
merely existence of `/dev/kvm`. A stopped workspace service is reported as
needing startup/namespace registration, and required proxy grants remain
unissued preparation needs. A required incompatible or unverifiable running host
service is a blocker under
[Services Compatibility](services.md#compatibility-and-replacement), not a
request to replace it. An absent interactive Pi fallback auth file is reported
as needing initialization. Service startup/registration and interactive Pi
fallback initialization are preparation needs, not blockers by themselves.
Missing credentials required by a noninteractive spawn remain blockers.

Inspection neither stages private keys or mount contents, creates or migrates Pi
auth, starts or stops services or containers, installs images, updates image
retention, repairs host configuration, nor writes an execution record. It is not
an execution attempt under `security.md`'s evidence contract. This read-only
guarantee applies to Wrix execution, not builds or downloads performed by a
surrounding `nix run` invocation.

Detected blockers and inconclusive required readiness checks, including probe
timeouts, return nonzero while retaining the reportable resolved plan. Invalid
inputs also return nonzero; inspection does not invent a plan it cannot resolve.
Probe time and captured output are bounded, and incomplete evidence is not
reported as successful readiness. Exit zero means no detected blockers, not
proof of guest firewall enforcement, workload execution, or future launch
success. Actual launch rechecks prerequisites against current state.

## Platform Implementations

### Linux (Podman)

- Rootless Podman is the default Linux runtime; krun is optional.
- The launcher grants temporary in-container `NET_ADMIN` for firewall setup on
  every launch, including `WRIX_NETWORK=open`, because baseline
  LAN/private/host-local/VPN blocking is always required. The immutable
  bootstrap installs the in-sandbox firewall ruleset (`nftables` by default on
  Linux Podman), disables/blocks IPv6 for v1, verifies policy, and replaces
  itself through `capsh` before workspace-controlled setup, the agent, or exit
  logging can execute.
- Default boundary runs as rootless **container-root** (no `--userns=keep-id`),
  which maps to the invoking host user. Nix sees real root-owned protected
  ancestors, store-mutating operations succeed, and workspace writes carry host
  UID/GID. Every image sets `IS_SANDBOX=1` so Claude accepts root execution. No
  UID-spoofing preload is used. The microVM path uses
  `--userns=keep-id:uid=0,gid=0 --user=0:0` (mapping the invoking host
  user/group to guest root) and enters the same trusted bootstrap through
  `krun-init.sh`; argv decoding occurs only after the capability drop.
- `--pids-limit 4096` fork-bomb guard
- `WRIX_MICROVM=1` selects `podman --runtime krun`; requested KVM must be
  present and usable, not merely an existing device path

### macOS (Apple `container` CLI)

- Requires macOS 26+ and Apple Silicon
- Virtualization.framework microVM, always (no separate container-mode path)
- vmnet networking with the same always-on in-guest firewall policy: no inbound
  ports, public-internet outbound in `open`, mandatory proxy egress in `limit`,
  LAN/private/host-local/VPN/special ranges blocked in both modes, IPv6
  disabled/blocked for v1. The Darwin host `pf` firewall is not part of the
  sandbox contract.
- An immutable `/network-bootstrap.sh` is the only stage granted `NET_ADMIN`; it
  uses image-pinned binaries, verifies the firewall, and `exec`s
  `/entrypoint.sh` through `capsh` after dropping `NET_ADMIN`. The agent
  entrypoint fails before workspace setup unless the trusted bootstrap marker
  exists and `NET_ADMIN` is absent from inheritable, permitted, effective,
  bounding, and ambient capability sets.
- VirtioFS workspace mount
- The ordinary caller-declared mount classifier handles `profile.mounts` and
  `SpawnConfig.mounts` uniformly — directories staged + copied at launch,
  regular files copy-from-parent-dir, Unix-socket sources rejected at launch
- The agent retains real guest-root identity, matching VirtioFS ownership and
  Nix\'s protected ancestors. There is no passwd rewrite or additional user
  namespace that changes Nix\'s ownership view.

This copy classifier does not define Wrix-managed credential/cache delivery.
[Pi credential storage](security.md#agent-api-credentials),
[launcher-owned Nix credential renewal](cache.md#launcher-owned-renewal) and
[managed tool-cache modes](profiles.md#shared-and-private-tool-cache-modes)
retain their own live visibility, durability and concurrency requirements;
launch-time snapshots alone cannot satisfy those contracts. This distinction
does not add write-back or synchronization to ordinary caller-declared mounts.

Known-host installation detaches a store-backed `/etc/ssh` directory and
replaces any known-host file symlink before writing. Runtime SSH setup preserves
baked Nix-store contents rather than modifying the OpenSSH package through a
rootfs symlink. [check](nix build
.#checks.x86_64-linux.known-hosts-runtime-store-purity) [check](nix build
.#checks.x86_64-linux.known-hosts-runtime-file-symlink)

### Darwin Shared Network

Acceptance: [Network Posture Acceptance](#network-posture-acceptance).

Apple service generations, client sandboxes and the Linux builder use one
explicit Wrix-owned network rather than assuming Apple's default subnet, bridge
or gateway. Creation/reuse and subnet validation are concurrent-safe, check
host/VPN and existing-network conflicts, and fail actionably rather than
silently mutating host routes. Routine start/connect does not invoke privileged
route repair or a wildcard publication fallback. Workspace stop preserves the
shared network and peer generations.

Network ownership does not authorize changing builder/service coexistence
policy, killing a serving deployment to realize an image, or starting extra VMs.
An incompatible existing environment is reported, not silently repaired through
disruptive runtime transitions. Real native route/SQL handshake evidence is
separate from network metadata and remains required.

## Success Criteria

- `mkSandbox` accepts the documented parameter set (`profile`, `cpus`,
  `memoryMb`, `deployKey`, `packages`, `mounts`, `env`, `runtimeSecrets`, `mcp`,
  `mcpRuntime`, `agent`, `agentPkg`, `agentSettings`) and returns
  `{ image, profile, devShell }` with generated configuration in
  `image.profileConfig` and no configured executable outputs.
  [check](verify:sandbox.data-only-constructor)
- Platform dispatch picks the Linux implementation on Linux hosts and the macOS
  implementation on Darwin hosts [check](verify:sandbox.platform-dispatch)
- Evaluating `mkSandbox` on an unsupported system throws at evaluation time
  rather than producing a broken derivation
  [check](verify:sandbox.unsupported-system-error)
- A built Linux sandbox starts a container and exits cleanly
  [system](verify:sandbox.linux-container-starts)
- A built macOS sandbox starts an Apple `container` microVM and exits cleanly
  [system](verify:sandbox.darwin-container-starts)
- The Darwin network bootstrap cannot resolve tools from `/workspace`, verifies
  the firewall before invoking `capsh`, preserves the agent argv, and enters
  stage two only after requesting an irreversible `NET_ADMIN` drop
  [system?](verify:sandbox.darwin-proxy-network-bootstrap)
- Linux container and krun initialization install the same open/limit policy and
  exact endpoint exceptions with image-pinned tools, preserve argv, and drop
  `NET_ADMIN` before workspace shims run during setup, agent execution, or exit
  logging [system?](verify:sandbox.linux-proxy-network-bootstrap)
- Both agent entrypoints reject a missing bootstrap marker or retained
  `NET_ADMIN` before setup or exit logging can run
  [system](verify:sandbox.entrypoint-requires-bootstrap)
- Files created inside `/workspace` carry the host UID/GID, not a
  container-internal UID [system](verify:sandbox.uid-mapping)
- Host filesystem outside `/workspace` and declared mounts is not visible inside
  the container [system](verify:sandbox.filesystem-isolation)
- `mounts` and `env` passed to `mkSandbox` are merged into the profile and reach
  the container as configured [system](verify:sandbox.custom-mounts-env)
- Every sandbox image carries the `wrix` CLI, so `wrix beads push` resolves
  inside the container without entering `nix develop` or running `nix run`
  [check](test-ci:test-wrix-cli-in-profile)
- In a fresh container built from a profile that ships `nix`, the runtime
  process (rootless container-root) runs `nix develop -c true`, a `nix build` of
  a flake target, and a store-mutating op against a baked root-owned path to
  completion (exit 0) with no `Operation not permitted` failure on a
  `/nix/store` path [system](verify:sandbox.nix-in-container)
- Every image sets `IS_SANDBOX=1` so Claude permits root execution without UID
  spoofing. [check](nix build
  .#legacyPackages.x86_64-linux.ciChecks.private-nix-artifacts)
- The default Linux launch command preserves real identity without injecting a
  UID-spoofing preload.
  [test](../crates/wrix-sandbox/tests/launch.rs::linux_default_boundary_preserves_real_identity)
- In a fresh container built from the selected image, the runtime user completes
  `nix-store --verify --check-contents` and an additive `nix build` without
  store-integrity failures. The baked store/database guarantee is owned by
  `image-builder.md` § In-Container Nix Store Consistency
  [system](verify:sandbox.nix-store-verify-clean)

### Network Posture Acceptance

- Darwin network planning shares an explicit Wrix network across generations,
  clients and builder, validates subnet/VPN conflicts without default-network
  assumptions, and preserves peer ownership without privileged route repair.
  [test?](../crates/wrix-sandbox/tests/apple_network.rs::owned_network_is_shared_conflict_checked_and_unprivileged)
- Real Apple shared-network clients complete authenticated SQL/cache and builder
  SSH requests without routine sudo/route repair, preserve peer network state
  and report actual conflict/coexistence limitations.
  [system?](test-ci:test-apple-owned-network)

- The launcher accepts exactly `WRIX_NETWORK=open` and `WRIX_NETWORK=limit`
  [test](command::launch::test::network_mode_parse_accepts_only_open_and_limit)
- Any other `WRIX_NETWORK` value errors through the production CLI before
  workspace services or a container start
  [test](../crates/wrix-cli/tests/sandbox_launch.rs::invalid_network_mode_fails_before_service_or_container_start)
- The configured network default is applied when `WRIX_NETWORK` is absent;
  explicit `open`/`limit` overrides take precedence
  [test](../crates/wrix-cli/tests/sandbox_launch.rs::profile_network_defaults_and_explicit_environment_precedence)
- Malformed network defaults and unsupported IPv6 policy fail before subprocess
  side effects, even with a valid environment override
  [test](../crates/wrix-cli/tests/sandbox_launch.rs::malformed_network_policy_fails_before_subprocesses_even_with_override)
- In `WRIX_NETWORK=open`, sandbox outbound to public internet succeeds, but
  outbound to LAN/private/host-local/VPN/special IPv4 ranges fails except for
  exact DNS and wrix-owned endpoint exceptions
  [system](verify:sandbox.network-open-blocks-lan)
- In `WRIX_NETWORK=limit`, the agent reaches only its designated proxy and exact
  Wrix data endpoints; authorized proxy traffic succeeds while direct public
  egress, public DNS and unapproved local destinations fail.
  [system?](verify:sandbox.network-limit-proxy)
- Both launch modes require a ready scoped proxy grant in restricted mode,
  including with Beads/cache disabled, and never select direct-egress fallback.
  [test?](../crates/wrix-cli/tests/proxy_policy.rs::limit_requires_ready_scoped_proxy_without_direct_fallback)
- IPv6 egress is disabled or blocked in both network modes for v1
  [system](verify:sandbox.network-ipv6-blocked)
- If firewall setup, IPv6 disablement, or `NET_ADMIN` drop cannot be verified,
  the launcher fails closed before the agent starts and never falls back to
  LAN-open networking [system](verify:sandbox.network-fail-closed)
- After startup, the agent process cannot modify firewall rules (for example
  `nft flush ruleset` on the nft backend, or the equivalent backend flush
  command, fails inside the running sandbox)
  [system](verify:sandbox.agent-lacks-net-admin)

### Runtime and Configuration Acceptance

- Run and spawn keep cache renewal owned by the original waiting launcher for
  the runtime child's lifetime, preserving stdio/TTY and observed exit
  semantics.
  [test?](../crates/wrix-sandbox/tests/cache_credentials.rs::both_launch_modes_own_renewal_while_waiting_for_runtime)
- Both modes supply a private read-only credential directory with a stable
  native Nix file reference that can observe host-side atomic refresh, without
  embedding tokens or issuer keys in environment values or
  ProfileConfig/SpawnConfig.
  [test?](../crates/wrix-sandbox/tests/cache_credentials.rs::both_launch_modes_mount_refreshable_private_auth_directory)
- Both run and spawn inject the cache-enabled Nix client freshness configuration
  owned by `cache.md`, without relying on a manual refresh or delivering host
  publication authority.
  [test?](../crates/wrix-sandbox/tests/cache_freshness.rs::both_launch_modes_inject_cache_freshness)

- `WRIX_MICROVM=1` selects
  `podman --runtime krun --userns=keep-id:uid=0,gid=0 --user=0:0` on Linux when
  `/dev/kvm` is available, uses the relay for interactive `run` and enters
  `krun-init.sh` directly for non-TTY `spawn`, serializes the requested command
  through `WRIX_KRUN_CMD`, passes terminal dimensions for interactive PTY setup,
  and reaches the krun-only init with real guest-root identity and working
  private Nix evaluation [system](verify:sandbox.linux-microvm-runtime)
- MicroVM opt-in fails with the typed missing-KVM error before Podman runtime
  dispatch when the KVM device is absent
  [system](verify:sandbox.linux-microvm-missing-kvm)
- `wrix run` errors at startup with a clear message when no valid Nix-generated
  `ProfileConfig` JSON is supplied
  [test](../crates/wrix-sandbox/tests/command.rs::run_requires_valid_profile_config)
- Actual host run/spawn use the generated image/configuration pair via the
  automatic devshell handoff or explicit `--profile-config`, without mutable
  legacy image defaults, a configured executable or runnable sandbox adapter.
  [system?](test-ci:test-profile-config-global-cli)
- `ProfileConfig.image` includes `ref`, `source`, explicit `source_kind`, and
  `digest`; the launcher/runtime installer rejects configs where `source_kind`
  is missing or incompatible with the selected platform install path
  [check](test-ci:test-profile-config-image-source-kind)
- Complete `ProfileConfig` parsing rejects malformed fields and unsupported
  schema versions before subprocesses
  [test](../crates/wrix-cli/tests/sandbox_launch.rs::malformed_profile_config_fails_before_subprocesses)
- Complete `ProfileConfig` parsing rejects duplicate JSON fields before
  subprocesses
  [test](../crates/wrix-cli/tests/sandbox_launch.rs::duplicate_profile_config_fields_fail_at_the_json_boundary)
- Complete `ProfileConfig` parsing permits opaque metadata only through its
  explicit `extensions` object and preserves platform-specific source semantics
  [test](../crates/wrix-sandbox/tests/config_contract.rs::explicit_profile_extensions_preserve_source_semantics)
- Image installer construction rejects empty source paths and contradictory
  runtime/source combinations before store operations
  [test](../crates/wrix-sandbox/src/image.rs::installation_constructor_rejects_contradictory_sources)
- The selected agent runtime comes from `ProfileConfig` and cannot be changed by
  caller env independently of the selected image/profile
  [test](../crates/wrix-sandbox/tests/command.rs::profile_config_agent_cannot_be_overridden_by_env)
- `wrix spawn --spawn-config <file>` parses the core `SpawnConfig` fields
  (`image_ref`, `image_source`, `image_source_kind`, `workspace`, `env`,
  `agent_args`, `mounts`) into the launch plan
  [test](../crates/wrix-sandbox/tests/spawn_config.rs::documented_spawn_config_fields_render_into_launch_plan)
- With or without `--stdio`, `wrix spawn` runs without a TTY or detachment,
  waits for the container command to finish, and returns its observed exit
  status; `--stdio` enables stdin rather than changing the lifecycle
  [test](../crates/wrix-sandbox/tests/launch.rs::spawn_waits_for_container_completion)
- `SpawnConfig.git` parses independent boolean grants, preserving omission as
  inheritance and explicit false as an override rather than collapsing them
  [test](../crates/wrix-sandbox/tests/spawn_config.rs::git_grants_preserve_omission_and_explicit_false)
- A `SpawnConfig.image_source` override requires an explicit source kind
  [test](../crates/wrix-sandbox/tests/spawn_config.rs::image_source_override_requires_source_kind)
- A `SpawnConfig.image_source_kind` override must be compatible with the current
  platform
  [test](../crates/wrix-sandbox/tests/spawn_config.rs::image_source_kind_must_match_platform)
- `SpawnConfig` cannot change the selected agent independently of
  `ProfileConfig`
  [test](../crates/wrix-sandbox/tests/spawn_config.rs::spawn_config_cannot_override_agent)
- Namespaced `SpawnConfig.extensions` payloads remain available unchanged to the
  in-container entrypoint through the read-only original config mount and
  `WRIX_SPAWN_CONFIG`
  [test](../crates/wrix-sandbox/tests/spawn_config.rs::consumer_extensions_are_mounted_for_entrypoint)
- On Linux, each `SpawnConfig.mounts` entry becomes a
  `-v <host_path>:<container_path>` podman argument, with `:ro` appended when
  `read_only: true`. A missing or empty `mounts` list produces no additional
  `-v` flags.
  [test](../crates/wrix-sandbox/tests/spawn_config.rs::linux_spawn_mounts_render_podman_volume_args)
- The packaged launcher does not mount the host Podman socket or export
  `CONTAINER_HOST` / `GC_HOST_*` by default; `WRIX_PODMAN_SOCKET` is ignored;
  and only `WRIX_UNSAFE_PODMAN_SOCKET` renders a real socket mount plus
  host-visible `CONTAINER_HOST` / `GC_HOST_*`, failing loudly when the socket is
  absent [system](verify:sandbox.unsafe-podman-socket)
- On Darwin, the same ordinary caller-declared mount classifier handles
  `profile.mounts` and `SpawnConfig.mounts` — one mechanism, not two.
  Directories are staged + copied at launch and regular files copy from their
  parent directory.
  [test](../crates/wrix-sandbox/tests/darwin_mounts.rs::mount_classifier_handles_profile_and_spawn_mounts_uniformly)
- On Darwin, mount entries whose `host_path` is a Unix socket fail before the
  container starts because VirtioFS does not pass socket operations
  [test](../crates/wrix-sandbox/tests/darwin_mounts.rs::mount_classifier_rejects_unix_sockets)
- Omitted `agent` selects Pi; explicit `direct` requires a package and a valid
  `meta.mainProgram` declaration rather than supplying a placeholder
  [check](verify:sandbox.agent-default-and-direct-contract)
- Both entrypoints execute the selected Pi, Claude, or declared direct
  executable; a non-Loom-named direct runner receives the original arguments and
  bidirectional stdio, and a missing direct executable fails clearly
  [system](verify:sandbox.entrypoint-declared-runner)
- Both launch modes read current repository policy on each launch and resolve
  independent Git grants from launch override, repository policy, then false,
  without treating key identity, host key presence, or `WRIX_GIT_SIGN` as a
  grant or policy override
  [test](../crates/wrix-sandbox/tests/launch.rs::git_grants_follow_override_repo_default_precedence)
- `wrix run` parses launcher options only before the workspace or `--`, uses CWD
  when the workspace is omitted, consumes the documented separator, and forwards
  the remaining agent arguments unchanged, including Wrix-looking option names
  and agent `--help`
  [test](../crates/wrix-cli/tests/sandbox_launch.rs::run_launch_options_stop_before_agent_arguments)
- Launch overrides reject conflicting flag pairs and malformed Git policy,
  including the unsupported `sign_commits` key, before services or credential
  staging
  [test](../crates/wrix-cli/tests/sandbox_launch.rs::invalid_git_policy_fails_before_side_effects)
- Before exec'ing the selected agent, the entrypoint rejects a mismatch between
  the ProfileConfig-selected `WRIX_AGENT` and the image-declared
  `/etc/wrix/image-agent`, then verifies the agent's binary is present and fails
  loudly with a clear error when it is absent from the image (e.g.
  `WRIX_AGENT=pi` against a claude image), rather than emitting a bare
  `command not found` [system](verify:sandbox.agent-binary-guard)
- Both entrypoints seed the selected agent's config home — claude `~/.claude`,
  pi `~/.pi/agent` — and make its documented workspace session location
  available [system](verify:sandbox.agent-config-homes)
- Granted deploy and signing keys are mounted independently and read-only at
  `/etc/wrix/keys/<name>` and `/etc/wrix/keys/<name>-signing`, respectively;
  child key environment variables point only at those mounted paths, and neither
  host-source paths nor `.pub` files are delivered
  [test](../crates/wrix-sandbox/tests/launch.rs::independent_git_grants_use_fixed_private_key_destinations)
- `ProfileConfig.security.deploy_key` accepts only a validated, single-component
  deploy-key name; absolute paths, separators, whitespace, and dot traversal
  fail during config parsing before credential staging
  [test](../crates/wrix-sandbox/tests/launch.rs::profile_config_rejects_unsafe_deploy_key_names_before_staging)
- Both entrypoints can derive the deploy public key from the mounted private key
  on demand with `ssh-keygen -y`
  [system](verify:sandbox.entrypoint-deploy-key-public)
- `agentSettings` merges into the selected agent's baked settings; direct
  accepts omission or `{}` and rejects nonempty settings at evaluation time
  [check](test-ci:test-sandbox-agent-settings)
- Pi's baked security-relevant defaults enable project trust inside the sandbox
  and disable install telemetry; update checking is a separate setting
  [check](test-ci:test-sandbox-agent-settings)
- Pi settings delivery preserves unspecified defaults and applies explicit
  consumer model, display, and tool-setting overrides without requiring any
  particular model ID or cosmetic value
  [check](verify:sandbox.pi-settings-precedence)
- Packaged Pi exposes additive codemode alongside read, Bash, edit, and write
  with no MCP servers configured, executes a built-in-tool script successfully,
  and honors an explicit consumer override disabling codemode
  [system](verify:sandbox.pi-codemode-tools)
- When `/workspace/bin` exists inside the container, it appears first on `PATH`,
  so a consumer-supplied shim at `/workspace/bin/<name>` resolves ahead of a
  same-named binary baked into the image
  [system](verify:sandbox.workspace-bin-path-present)
- When `/workspace/bin` does not exist, the container's `PATH` does not contain
  `/workspace/bin` [system](verify:sandbox.workspace-bin-path-absent)
- Both platform entrypoints implement the `/workspace/bin` PATH prepend
  [system](verify:sandbox.entrypoint-workspace-bin-prepend)
- The packaged runtime image installer's digest-presence check determines
  whether the selected source's content digest matches an image already in the
  platform store before invoking the install pipeline; on a digest hit, no image
  source is executed, no tar bytes are streamed, and no `*-load` CLI is invoked
  [system](test-ci:test-image-install-digest-skip)
- On Linux, the runtime image installer dispatches the Linux source kind defined
  by `image-builder.md` through an archive-less descriptor-to-OCI-layout install
  path (`oci:<oci_layout>:<oci_ref>` → `containers-storage:<ref>` with skopeo,
  or equivalent wrix); the docker/OCI archive conversion path is not used for
  Linux descriptor sources
  [test](../crates/wrix-sandbox/tests/image_install.rs::linux_descriptor_sources_use_archiveless_install_path)
- The packaged Linux launcher supplies a readable OCI layout to skopeo with the
  Podman store destination and skips installation when that image is already
  present [system](test-ci:test-image-install-real-skopeo)
- The digest-presence check also works when the digest comes from the descriptor
  rather than ProfileConfig
  [test](../crates/wrix-sandbox/tests/image_install.rs::descriptor_digest_preflight_works_without_profile_digest)
- A second spawn of an already-loaded image performs no writes to the platform
  store's layer directory and does not execute the image source
  [test](../crates/wrix-sandbox/tests/image_install.rs::already_loaded_image_performs_no_store_writes)
- Run/spawn preserve digest reuse and necessary installation without broadly
  pruning cached images, including when more than eight images are present.
  [test](../crates/wrix-sandbox/tests/image_gc.rs::launch_installation_does_not_prune_cached_images)
- Image information is read-only: it does not install/prune images or write
  usage/retention bookkeeping.
  [test](../crates/wrix-sandbox/tests/image_gc.rs::image_inventory_is_read_only)
- Runtime listings parse into typed store targets
  [test](../crates/wrix-sandbox/src/image.rs::podman_rows_parse_typed_references_ids_and_absent_fields)
- Explicit image collection preserves existing-container references and
  in-flight install-to-container-create reservations without forced container
  removal, including concurrent launches from different workspaces.
  [test?](../crates/wrix-sandbox/tests/image_gc.rs::gc_preserves_existing_container_and_inflight_images)
- Usage checks include stopped containers, compare real image identities rather
  than age/tags alone and do not delete when reference information is
  unreliable; an idle configured image is not automatically considered in use.
  [test?](../crates/wrix-sandbox/tests/image_gc.rs::unused_checks_use_runtime_identity_and_fail_closed)
- Image inspection/removal failures are accumulated while other independently
  safe images are processed; uncertain candidates are not deleted and completed
  removals are not presented as rolled back.
  [test?](../crates/wrix-sandbox/tests/image_gc.rs::gc_continues_safe_images_after_inspection_or_removal_failure)
- Explicit GC selects unused Wrix-owned images across profile/service/support
  kinds, including recent/current idle images; names, dangling status or the
  former MRU list do not authorize deletion of ambiguous/unowned images or pin
  eligible images.
  [test?](../crates/wrix-sandbox/tests/image_gc.rs::gc_only_selects_owned_unused_images_across_kinds)
- Image-only GC and combined `gc all` use the same image policy; workspace/host
  data GC does not silently prune images or widen collection authority.
  [test?](../crates/wrix-service/tests/host_service.rs::gc_all_combines_host_data_and_same_image_collector)
- Real Podman image GC removes eligible Wrix images while retaining unrelated,
  ambiguous and container/in-flight-protected images in an isolated test store.
  [system?](test-ci:test-image-gc-podman)
- Real Apple image GC removes eligible Wrix images while retaining unrelated,
  ambiguous and container/in-flight-protected images in an isolated test store.
  [system?](test-ci:test-image-gc-apple)
- Apple `container list` records are parsed for image references and descriptor
  IDs so cleanup preserves images used by existing Apple containers
  [test](image::test::apple_container_list_preserves_images_used_by_existing_containers)
- Apple image digest inspection accepts prefixed digests, bare digests, and
  content-digest IDs, without treating opaque runtime IDs as digest evidence
  [test](../crates/wrix-sandbox/src/image.rs::apple_content_digest_accepts_prefixed_bare_and_id_fallback_variants)
- Runtime MCP selection reaches the container through `WRIX_MCP`
  [test](../crates/wrix-sandbox/tests/launch.rs::runtime_mcp_selection_reaches_entrypoint)
- Explicit and runtime selection publish the same schema-v1 manifest for all
  agent kinds, preserve server command/arguments/environment mapping in native
  Claude and Pi configuration, and hand the manifest unchanged to direct runners
  [system](verify:sandbox.mcp-manifest-handoff)
- Packaged Pi receives only the selected Wrix-managed entries, including
  none/all and unknown-name handling, uses native names and codemode exposure,
  replaces stale generated configuration on launch, leaves host configuration
  untouched, and independently honors native trusted-project MCP precedence
  [system](verify:sandbox.pi-mcp-selection)
- Packaged Pi codemode receives structured MCP results without Wrix flattening
  or size-dependent shape changes; large results remain available to scripts
  under Pi's native result contract
  [system](verify:sandbox.pi-mcp-structured-results)
- Packaged Pi can forward an MCP image block from a codemode call to its result
  without converting the image to prose [system](verify:sandbox.pi-mcp-images)
- Packaged Pi preserves MCP tool errors as `isError` results in codemode and
  propagates protocol/transport failures and cancellation rather than reporting
  success or replaying side-effecting calls through a Wrix client
  [system](verify:sandbox.pi-mcp-errors-cancellation)
- Native Pi owns selected stdio-server shutdown, including child processes
  [system](verify:sandbox.pi-mcp-lifecycle)
- Wrix images contain no custom Pi MCP protocol client
  [system](test-ci:test-pi-mcp-image-wiring)
- On Darwin, the runtime image installer converts the Darwin source kind defined
  by `image-builder.md` to a temporary OCI archive before invoking
  `container image load --input <oci-archive>`, then removes the temporary
  archive and relies on the digest-presence check while per-blob install remains
  out of scope [system](verify:sandbox.darwin-image-load)

### Project Configuration Acceptance

- Run/spawn inspection and actual dispatch resolve the same explicit versus
  generated-default configuration, with no repository search/evaluation or
  implicit image when neither is supplied.
  [test](../crates/wrix-cli/tests/host_configuration.rs::inspection_and_launch_share_project_configuration_resolution)
- Invalid or unsupported declared configuration fails before preparation rather
  than falling back to another executable, image or schema reader.
  [test](../crates/wrix-cli/tests/host_configuration.rs::unsupported_project_configuration_fails_before_preparation)
- Real constructor-generated configuration reaches the provided host CLI with
  its paired image/agent metadata, without configured launcher outputs.
  [system?](test-ci:test-profile-config-global-cli)

### Tool Cache Launch Acceptance

- Spawn parsing preserves omitted tool-cache sharing as inheritance, explicit
  false as private and true as shared, with strict owned object/boolean
  validation before preparation.
  [test](../crates/wrix-sandbox/tests/spawn_config.rs::tool_cache_sharing_parses_owned_optional_boolean)
- Run cache flags obey launcher prefix/conflict rules without interpreting
  forwarded agent arguments or accepting long-name launcher aliases.
  [test](../crates/wrix-cli/tests/tool_cache_controls.rs::run_cache_flags_follow_prefix_and_conflict_rules)
- Both dry-run renderings report the shared resolver's effective tool-cache mode
  for run/spawn without creating, reading prewarms into or populating cache
  data.
  [test](../crates/wrix-cli/tests/preflight.rs::preview_reports_effective_tool_cache_mode_without_preparation)

- Restricted Rust with resolved shared mode fails before preparation in
  run/spawn; effective private mode remains valid with Nix/Celler enabled, and
  no shared compiler endpoint is granted.
  [test](../crates/wrix-cli/tests/tool_cache_controls.rs::restricted_rust_requires_effective_private_tool_caches)
- Both preflight renderings retain the resolvable plan and explain the
  restricted Rust/private-cache conflict as a nonzero blocker without starting
  services, staging credentials or allocating compiler/cache views.
  [test](../crates/wrix-cli/tests/preflight.rs::restricted_rust_cache_conflict_is_read_only_and_actionable)

### Strict Configuration Acceptance

- Structured `profile.network_allowlist` host/ports objects round-trip through
  the Rust boundary, reject unknown rule fields, and match the documented JSON.
  [test](../crates/wrix-sandbox/tests/config_contract.rs::profile_network_rule_objects_match_documented_wire)
- `ProfileConfig` rejects unknown root and Wrix-owned nested fields before
  preparation while recognizing the complete supported Nix-generated shape.
  [test](../crates/wrix-sandbox/tests/config_contract.rs::profile_owned_structures_reject_unknown_fields)
- `SpawnConfig` rejects unknown root and Wrix-owned nested fields before
  preparation, including former flat consumer payload fields.
  [test](../crates/wrix-sandbox/tests/config_contract.rs::spawn_owned_structures_reject_unknown_fields)
- Both configs accept an omitted or object-valued top-level `extensions`, reject
  other extension-container types, and leave its contents opaque.
  [test](../crates/wrix-sandbox/tests/config_contract.rs::explicit_extensions_are_optional_opaque_objects)
- Extension contents cannot alter any resolved Wrix launch field.
  [test](../crates/wrix-sandbox/tests/config_contract.rs::extension_payloads_do_not_override_launch_policy)
- Unknown-field rejection preserves validated open environment/runtime-secret
  maps, defaults, omission rules and duplicate-field rejection.
  [test](../crates/wrix-sandbox/tests/config_contract.rs::strict_boundaries_preserve_existing_map_and_default_semantics)
- Real Nix-generated built-in configuration is accepted by the Rust parser with
  the intended declared fields and platform-specific source semantics.
  [system](test-ci:test-profile-config-parser-agreement)
- The documented namespaced `SpawnConfig` example parses through the Rust
  boundary and requires no formal schema validator.
  [test](../crates/wrix-sandbox/tests/config_contract.rs::documented_spawn_example_matches_parser)

### Network and Mount Enforcement Acceptance

- Live rootless Podman launches satisfy the network controls above, including
  open/limit policy changes, known-live positive and negative controls, and
  exact open-mode DNS, restricted-mode proxy and Wrix-managed endpoint scope.
  [system?](verify:sandbox.network-controls-podman)
- Live krun launches satisfy the same network controls through the production
  microVM launch path, with their own KVM/runtime prerequisites and results.
  [system?](verify:sandbox.network-controls-krun)
- Live Apple Container launches satisfy the same network controls through the
  production Darwin launch path, with their own host prerequisites and results.
  [system?](verify:sandbox.network-controls-apple)
- Live rootless Podman workloads reject writes through read-only profile and
  spawn file/directory declarations, preserve read-only host sources, and permit
  corresponding writable controls.
  [system](verify:sandbox.mount-controls-podman)
- Live krun workloads satisfy the same mount controls through the production
  microVM launch path, with their own KVM/runtime prerequisites and results.
  [system](verify:sandbox.mount-controls-krun)
- Live Apple Container workloads satisfy the same mount controls without
  claiming new directory write-back persistence, with their own host
  prerequisites and results. [system?](verify:sandbox.mount-controls-apple)

### Launch Preflight Acceptance

- Default human-readable and explicit `--dry-run --json` output describe the
  same plan and findings; JSON includes a schema version and stable finding
  codes and is not mixed with probe output, including on readiness failure.
  [test](../crates/wrix-cli/tests/preflight.rs::dry_run_renderings_share_a_versioned_report)
- `--json` requires `--dry-run` in the launcher-option position and obeys the
  existing boundary for forwarded agent arguments; it does not select agent
  stdio behavior.
  [test](../crates/wrix-cli/tests/preflight.rs::dry_run_json_is_only_a_preview_rendering_option)
- Both renderings, findings and inspection diagnostics omit environment values,
  agent argument contents, opaque consumer payloads and credential contents,
  including when invalid inputs or probe failures are reported.
  [test](../crates/wrix-cli/tests/preflight.rs::dry_run_summaries_do_not_dump_sensitive_inputs)
- Production `run` recognizes `--dry-run` only in its launcher-option prefix,
  preserves occurrences in agent arguments, and `spawn` accepts it with
  `--spawn-config` and `--stdio` without executing the agent.
  [test](../crates/wrix-cli/tests/preflight.rs::dry_run_parses_explicit_launch_options)
- Retired dry-run environment variables neither select inspection nor reject an
  invocation, and the service-binary override cannot redirect service dispatch.
  [test](../crates/wrix-cli/tests/preflight.rs::dry_run_ignores_retired_environment_switches)
- Inspection and launch resolve the same profile, image, agent, mount intents,
  network policy, resources and independent Git grants from the same inputs,
  including repository-policy and invocation-override precedence.
  [test](../crates/wrix-cli/tests/preflight.rs::dry_run_reports_launch_policy_and_provenance)
- Credential inspection reports names, selected sources and availability without
  reading credential-file contents or disclosing built-in or declared
  runtime-secret values.
  [test](../crates/wrix-cli/tests/preflight.rs::dry_run_reports_credential_availability_without_values)
- Both inspection modes leave auth storage, private keys, mount contents,
  workspace records and host configuration unchanged, create no staging
  directories, and invoke no service/container start or stop, image installation
  or retention update.
  [test](../crates/wrix-cli/tests/preflight.rs::dry_run_is_read_only)
- Readiness inspects the selected runtime and applicable required mount and
  credential sources without provisioning them, and reports detected blockers.
  [test?](../crates/wrix-cli/tests/preflight.rs::dry_run_checks_selected_launch_prerequisites)
- A requested microVM reports inaccessible or unusable KVM as a blocker even
  when the device path exists.
  [test?](../crates/wrix-cli/tests/preflight.rs::dry_run_checks_kvm_access_not_only_presence)
- Missing services and absent interactive Pi fallback auth are reported as
  preparation needs rather than automatically provisioned or rejected; missing
  required noninteractive auth remains a blocker.
  [test?](../crates/wrix-cli/tests/preflight.rs::dry_run_distinguishes_preparation_from_blockers)
- A required incompatible/unverifiable running shared service produces a nonzero
  readiness report without registration, replacement or grant issuance; a
  missing/stopped service remains a preparation need, and launch rechecks.
  [test?](../crates/wrix-cli/tests/preflight.rs::dry_run_reports_shared_service_incompatibility_without_repair)
- Unallocated endpoints and preparation-only paths remain explicitly unresolved
  rather than fabricated.
  [test?](../crates/wrix-cli/tests/preflight.rs::dry_run_preserves_unresolved_preparation_details)
- Invalid inputs, detected blockers and inconclusive required checks return
  nonzero; blocker reports retain the configuration that could be resolved.
  [test?](../crates/wrix-cli/tests/preflight.rs::dry_run_reports_blockers_with_nonzero_status)
- Readiness probes enforce time and captured-output bounds, disclose incomplete
  evidence, and never turn a timeout or incomplete required check into success.
  [test?](../crates/wrix-cli/tests/preflight.rs::dry_run_probe_limits_are_enforced_and_reported)
- A successful preview states that guest enforcement and workload execution
  remain unverified and does not assert guaranteed launch success.
  [test?](../crates/wrix-cli/tests/preflight.rs::dry_run_distinguishes_readiness_from_enforcement)
- Real launches recheck prerequisites against current state rather than trusting
  an earlier preview.
  [test?](../crates/wrix-cli/tests/preflight.rs::launch_rechecks_readiness_without_reusing_preview)

## Requirements

### Functional

1. **mkSandbox API** — accepts the parameters above; returns
   `{ image, profile, devShell }` with `image.profileConfig`. Profile schema
   lives in `profiles.md`; image build in `image-builder.md`; MCP server
   contracts in `playwright-mcp.md`; native terminal debugging in `tmux.md`.
2. **Platform dispatch** — Linux selects the Podman launcher; macOS selects the
   Apple `container` CLI launcher; unsupported systems throw.
3. **Workspace mount** — the selected workspace bind-mounts at `/workspace`;
   profile mounts merge on top.
4. **UID mapping** — files created in `/workspace` carry host UID/GID.
5. **Custom mounts and env** — `mkSandbox`'s `mounts`, non-secret `env`, and
   `runtimeSecrets` extend the profile rather than replace it. Runtime-secret
   maps right-merge by environment name; values are resolved only by the
   launcher.
6. **Git key identity and delivery** — `deployKey` supplies a default validated
   key name, overridden by repository `git.deploy_key`; without either, the name
   is derived as specified by `cli.md`. Identity does not grant credentials. An
   effective deploy grant mounts only the deploy key at `/etc/wrix/keys/<name>`;
   an effective sign grant mounts only the signing key at
   `/etc/wrix/keys/<name>-signing`. Each mounted key gets its corresponding
   child `WRIX_DEPLOY_KEY` or `WRIX_SIGNING_KEY` value. Public keys can be
   derived with `ssh-keygen -y`; `.pub` files are not mounted. Host-source
   resolution and missing-key behavior belong to `security.md`.
7. **MCP opt-in** — Wrix owns registry selection and the
   manifest-to-native-config translation described in Architecture. Agent
   clients own protocol handling and presentation; external direct runners own
   manifest consumption. Profile output naming remains in `profiles.md`.
8. **Agent runtime axis** — Selection is build-time and immutable, with Pi as
   default and an explicit consumer package for direct. Configuration delivery,
   native Pi codemode, security-relevant defaults, and executable guards follow
   Architecture. Only the selected agent receives its config. `agentSettings`
   merges into native settings; direct accepts only omitted or empty settings.
   Credential delivery belongs to `security.md`. Image composition belongs to
   `image-builder.md`.
9. **Launcher contract** — `wrix run` reads immutable Nix-generated
   `ProfileConfig` JSON plus CLI/host-env runtime inputs; `wrix spawn` reads the
   same `ProfileConfig` plus per-launch `SpawnConfig` JSON. Both share container
   construction, including workspace service startup and endpoint injection when
   services are enabled. Data-generation and host-handoff rules are owned by
   _Architecture > Sandbox Outputs_; workspace service contracts are owned by
   `services.md`, and project-cache transport/trust and
   [client freshness](cache.md#client-publication-freshness) by `cache.md`.
10. **Image source dispatch** — image install dispatches on the explicit source
    kinds owned by `image-builder.md`, not filename or platform guessing. A
    per-launch `image_source` override must carry a matching
    `image_source_kind`; it may not silently inherit an incompatible kind from
    `ProfileConfig`. The selected source's digest is derived or validated before
    the installer's digest-presence query; preview never installs the source.
11. **Explicit image cleanup** — normal launch/service installation does not
    broadly prune images. Read-only inventory, manual collection, protected
    container/in-flight images and import-reference housekeeping follow
    [Image Retention and Cleanup](#image-retention-and-cleanup); MRU-eight is
    not an automatic deletion requirement.
12. **Per-launch mounts via SpawnConfig** — `wrix spawn`'s `SpawnConfig.mounts`
    adds per-launch bind mounts on top of `profile.mounts` and `mkSandbox`'s
    `mounts`. Each entry maps `host_path → container_path` with
    `read_only: true` rendering `:ro`. On Linux this is a literal `-v` flag. On
    Darwin, ordinary caller-declared `SpawnConfig.mounts` flows through the same
    classifier as `profile.mounts`: directories staged + copied, regular files
    copy-from-parent-dir, Unix-socket sources rejected at launch with a clear
    error (VirtioFS does not pass socket operations). Managed credential/cache
    delivery follows the separate contracts linked in
    [macOS](#macos-apple-container-cli). Read-only preflight inspects required
    source availability; actual launch rechecks current prerequisites rather
    than treating an earlier preview as authorization.
13. **Workspace `bin/` PATH prepend** — When `/workspace/bin` exists inside the
    container, both Linux and macOS entrypoints prepend it to `PATH` so
    consumer-supplied shims under the workspace's `bin/` resolve ahead of
    image-baked binaries with the same name. The check is directory existence,
    not per-binary; the consumer owns what it ships in `bin/`. When the
    directory is absent, `PATH` is unchanged. The contract is PATH ordering only
    — wrix does not create `/workspace/bin`, does not validate its contents, and
    does not allowlist individual shims.
14. **In-container Nix** — a sandbox built from a `nix`-shipping profile lets
    the runtime user run both additive (`nix develop`, `nix build` of new
    closures) and store-mutating (replace, GC, delete of baked paths) Nix
    operations without permission or store-integrity failures. On the default
    boundary the runtime process is rootless container-root, which maps to the
    host user that owns the baked store, so it can mutate root-owned store paths
    — the `deletePath → fchmodat2(u+w)` primitive no longer hits `EPERM`. This
    spec owns that runtime identity and permissions boundary; `image-builder.md`
    § In-Container Nix Store Consistency owns the baked store/database
    invariant.

### Non-Functional

1. **Rootless / no elevated privileges** — Linux runs rootless Podman; macOS
   runs the Apple `container` CLI as the calling user. No host capabilities are
   granted by default; `WRIX_UNSAFE_PODMAN_SOCKET` is an explicit unsafe opt-in
   outside the normal sandbox boundary.
2. **Boundary class** — macOS is always microVM; Linux defaults to rootless
   container, opts into microVM with `WRIX_MICROVM=1` (see `specs/security.md`).
3. **Network posture** — modes, exact exceptions, protected addresses and
   fail-closed startup follow [Network Posture](#network-posture). Restricted
   destination admission and its TLS limitations belong to `proxy.md`.

## Out of Scope

- Configured per-project Wrix executables, `wrix-run`/runnable sandbox adapters,
  retired `package`/`launcher` attributes, or executable fields in profile
  manifests; project consumers provide image/configuration data to host Wrix.

- Performance SLAs or fixed per-VM memory-overhead guarantees; runtime costs
  require measurement and are not promised by this contract
- Standalone formal `ProfileConfig`/`SpawnConfig` schema artifacts or generation
  toolchains, including JSON Schema and CUE; Rust parsing and producer-agreement
  tests are authoritative
- Fuzzing infrastructure and campaigns; focused malformed-input and
  duplicate-field regression tests remain required
- Runtime network-denial telemetry and its collectors, storage or diagnostic
  lifecycle; this does not remove `security.md`'s generic permission for
  explicitly enabled component diagnostics
- Resource-limit policy redesign and new workload-level CPU, memory or
  process-limit stress/benchmark suites; existing controls are delegated to the
  selected runtime rather than independently assured by this enforcement pass
- Host repair or provisioning during launch preflight; inspection reports needs
  without performing them
- Proving guest enforcement or workload success through read-only inspection;
  these require assembled-system execution
- Compatibility interfaces for retired dry-run environment switches
- A Wrix-managed detached launch mode or background completion observer; callers
  retain supervision of foreground launchers
- Shared writable guest stores/databases and implicit private-store persistence.
  Admitted service-owned installed-store reuse belongs to
  [Service-Owned Nix Stores](store.md), not ordinary profile mounts. Private
  mode remains image-seeded and disposable.
- Windows support
- GPU passthrough
- Inbound port forwarding
- User-defined unsafe networking modes; wrix does not provide a LAN-open escape
  hatch
- Sharing an agent sandbox between operators; host-user service workspace
  namespaces are a separate boundary owned by `services.md`
- Managed-environment APIs, external-harness tool routing, and Pi Durable
  integration; whole-agent sandboxing is the supported model for this scope
- Wrix-owned codemode interpreters, classifier workflows, and codemode-only
  defaults; Pi owns tool composition, and consumers may configure it natively
- Compatibility aliases for retired tmux MCP tools or Wrix's Pi MCP client
