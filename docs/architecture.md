# Architecture

Wrix is a secure sandbox for running AI coding agents in isolated containers. It
provides container isolation on Linux (Podman) and macOS (Apple container CLI),
with built-in support for Pi and [Claude Code](https://claude.ai/code) (from
nixpkgs) and a direct agent slot for consumer-supplied runners, including
external orchestrators such as [Loom](https://github.com/taheris/loom), plus
tooling for notifications, remote Nix builds, and integration hooks.

This is an overview of the intended architecture. The
[spec index](README.md#specs) locates each owning contract and its acceptance
criteria; pending criteria are not evidence that an integration is implemented
or verified. Component mechanics and defaults are defined there, not
independently by this overview.

## Design Principles

1. **Explicit trust boundaries** — Worker filesystem/process isolation and the
   separately trusted shared service have different roles; delegated compiler
   execution is an explicit capability inside the service boundary
2. **Worker least privilege** — Trusted startup establishes networking and drops
   setup capabilities before agent execution
3. **Coherent execution identity** — Workspace writes retain correct host
   ownership while Nix sees its real root-owned protected ancestors
4. **Internet egress with local-network isolation** — Public internet is
   available in `open` mode, while LAN/private/host-local/VPN access is blocked
   in every mode
5. **Nix all the way down** — Config, image sources, and the launcher are
   deterministic Nix outputs
6. **Agent-runtime axis is orthogonal to the profile axis** —
   `direct`/`claude`/`pi` compose with `base`/`rust`/`python` rather than
   multiplying out

## Platform Support

| Platform | Container Technology                           | Networking                                                                                 |
| -------- | ---------------------------------------------- | ------------------------------------------------------------------------------------------ |
| Linux    | Podman rootless, optional krun microVM         | public internet egress with in-sandbox firewall blocking LAN/private/host-local/VPN ranges |
| macOS    | Apple container CLI + Virtualization.framework | vmnet bridge with the same in-guest firewall policy                                        |

Both platforms mount `/workspace` read-write with correct file ownership.
Sandbox networking is fail-closed: wrix does not provide a LAN-open escape
hatch.

## Source Layout

```
crates/
├── wrix-core/           # Shared Rust types: paths, workspace identity, config schemas, errors
├── wrix-notify/         # Host attention queue, focus checks, and terminal navigation
├── wrix-cli/            # Human-facing `wrix` binary: run, spawn, service, beads
├── wrix-sandbox/        # Rust host orchestration for container launch
├── wrix-service/        # Host-user service lifecycle, workspace namespaces, compiler contexts
├── wrix-cache/          # Celler integration, selected host publication, and hook support
└── wrix-beads/          # Passive workspace information and explicit beads synchronization

lib/
├── default.nix          # Top-level API: mkSandbox, profiles, mkProfileImages
├── sandbox/             # Container isolation
│   ├── default.nix      # Platform dispatcher, MCP integration, mkProfileImages
│   ├── profiles.nix     # Built-in profiles (base, rust, python)
│   ├── image.nix        # OCI image source builder; selects agent runtime layer
│   ├── manifest.nix     # Profile→image JSON manifest consumed by orchestrators
│   ├── linux/           # Podman implementation + krun microVM support
│   ├── darwin/          # Apple container implementation
│   └── builder/         # Static-busybox bootstrap entrypoint for the Linux builder
├── services/            # Shared Dolt/Celler/proxy/sccache packaging and host integration
├── mcp/                 # MCP server registry
│   ├── default.nix      # Server registry: { playwright }
│   └── playwright/      # Playwright MCP server
├── prek/                # Pre-commit hook shims
├── builder/             # macOS-side CLI for the Linux remote builder
├── notify/              # Desktop notifications
└── util/                # Shared utilities (container CLI shim, SSH, paths, …)

docs/
├── README.md            # Project overview, terminology
├── architecture.md      # This file
├── tmux.md              # Native terminal workflow (also /etc/wrix/tmux.md in images)
├── spec-conventions.md  # Spec-authoring conventions
└── style-rules.md       # Code standards (SH-, NX-, DOC-, GIT-, TST-, RS-, COM-, CLI-)
```

## Component Overview

| Component            | Purpose                                                                                                                                                                      | Entry Point                                                    |
| -------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------- |
| Sandbox              | Container creation and lifecycle                                                                                                                                             | `mkSandbox`, `wrix run`/`spawn`                                |
| Profiles             | Pre-configured dev environments                                                                                                                                              | `profiles.{base,rust,python}`                                  |
| Agent Runtime        | Single selected agent baked into the image and exec'd by its entrypoint                                                                                                      | `mkSandbox { agent = …; }` plus generated launch configuration |
| MCP Servers          | Optional capabilities exposed through native agent clients                                                                                                                   | `mcp.playwright`, `mcpRuntime = true`                          |
| Image Source Builder | OCI image source generation via Nix (`nix-descriptor` on Linux, `docker-archive` on Darwin)                                                                                  | `lib/sandbox/image.nix`                                        |
| Profile Config       | Immutable JSON config for the Rust launcher: image ref/source/source kind, agent, mounts, non-secret env, runtime-secret names/policies, network allowlist, service features | `ProfileConfig` generated by `mkSandbox`                       |
| Profile Manifest     | JSON map of profile → profile config/image metadata for orchestrators                                                                                                        | `packages.profile-images` (`mkProfileImages`)                  |
| Host-user Services   | Shared infrastructure lifecycle with logical workspace namespaces                                                                                                            | [`wrix service ...`](../specs/services.md)                     |
| Project Nix Cache    | Authorized common/private Celler views, selected host publication and native age eviction                                                                                    | [`wrix service cache ...`](../specs/cache.md)                  |
| Egress Proxy         | Authenticated HTTP/CONNECT admission for restricted workers                                                                                                                  | [`proxy.md`](../specs/proxy.md)                                |
| Shared Compiler      | Actual sccache daemon executing eligible worker compilation in the service                                                                                                   | [`services.md`](../specs/services.md#shared-compiler-service)  |
| Notifications        | Desktop alerts when the agent waits                                                                                                                                          | `wrix-notify`, `wrix-notifyd`, `wrix-focus-notification`       |
| Linux Builder        | Remote Nix builds on macOS                                                                                                                                                   | `wrix-builder`                                                 |

## Rust Boundary Types

Inputs are parsed by their owning component before orchestration. `wrix-core`
owns workspace hashes, cache public keys (including decoded Ed25519 key length),
and the shared YAML reader that produces a relative Git `Branch` for beads and
service planning. Service plans retain that branch and represent Dolt endpoints
as Unix/TCP payload variants, with nonzero TCP ports.

The sandbox parses closed Wrix-owned JSON structures; consumer-owned opaque
payloads belong only in explicit `extensions` objects. The schema and real
Nix-producer/parser agreement are owned by
[Sandbox Configuration](../specs/sandbox.md#strict-configuration-boundaries).
Image sources pair a nonempty path with its format; installer construction
selects only supported runtime/source combinations. Image-store APIs carry
`Digest`, `ImageRef`, `ImageId`, and typed lookup targets through
digest-presence checks, retagging and explicit cleanup. Text rendering stays at
subprocess, logging and serialization boundaries. Filesystem availability,
container state and network readiness remain runtime checks.

## Sandbox Launcher

Host consumers provide one Nix-managed `wrix`, normally through Home Manager,
nix-darwin or `nix profile add 'github:taheris/wrix#wrix'`; explicit temporary
Nix provision is also supported. No particular installation directory is
required. Installing/updating the host package does not implicitly update
services. [CLI Host Setup](../specs/cli.md#host-cli-and-project-configuration)
owns this workflow.

Projects construct one `mkSandbox` object and use `sandbox.devShell { }`. The
constructor supplies an immutable launch-config pointer rather than adding
another Wrix executable to PATH or overriding the host service-image binding.
The Wrix and Loom checkouts can therefore select different project images while
`which wrix` stays the same. Missing/incompatible host setup blocks Wrix
activation effects with installation guidance, not an implicit install or
second-binary fallback.

Pure `image-<profile>[-<agent>]` outputs and the data-only
`packages.profile-images` manifest remain. `packages.wrix` and
`packages.default` select the host CLI. Configured `sandbox-*`/default/debug
launcher packages, `wrix-run`, `mkSandbox.package`/`launcher` and manifest
executable fields are not compatibility interfaces. Programmatic consumers pass
the selected generated `ProfileConfig` explicitly to host Wrix.

The launcher exposes two subcommands sharing the same Rust-owned container
construction (mounts, env passthrough, independent Git key grants, service
startup, network firewall policy):

- `wrix run [LAUNCH_OPTIONS] [DIR] [AGENT_ARGS…]` — interactive (TTY). Reads
  immutable image/profile/agent defaults from explicit or generated project
  `ProfileConfig` JSON and runtime inputs from CLI/env.
- `wrix spawn --spawn-config <file> [--stdio]` — programmatic dispatch. Reads
  the same `ProfileConfig` plus per-launch workspace, env pairs, image override,
  mounts, and agent args from JSON `SpawnConfig`. `--stdio` adds `WRIX_STDIO=1`
  so the selected agent uses its stdio protocol
  (`claude --input-format stream-json`, `pi --mode rpc`).

Each launch reads the selected workspace repository's current `wrix.toml`.
Deploy and signing grants resolve independently from invocation override,
repository policy, then false; outside a repository, the policy tier is absent.
Profiles select key identity, not permission. Only granted keys are resolved,
before services or credential staging: an explicit host pointer is the sole
candidate, otherwise the selected managed-key fallback is required. Ambient keys
and `WRIX_GIT_SIGN` do not grant credentials. Only granted private files are
staged read-only under `/etc/wrix/keys`, with child key environment values
pointing at those container destinations, never the host sources.

Explicit `run --dry-run` and `spawn --dry-run` inspect the resolved plan and
bounded local readiness without staging, provisioning, issuing grants or
executing an agent. Human and optional versioned JSON reports omit sensitive
inputs. This [launch preflight](../specs/sandbox.md#read-only-launch-preflight)
is distinct from the installer's image digest-presence check; only actual launch
installs a missing image and establishes an execution record.

See [sandbox.md](../specs/sandbox.md) and [profiles.md](../specs/profiles.md)
for the full launcher and manifest contracts.

## Host Services and Build Caching

[Services](../specs/services.md#architecture) owns one deployment per host user,
with Dolt, Celler, the egress proxy and one active shared sccache daemon. A
small [host controller](../specs/services.md#host-control-service) owns stable
Rama role routing, namespace policy/issuance, deployment ownership and
post-client retirement outside replaceable generations. It is not an agent
supervisor, general command runner or host-store service; management is
authenticated local control, never a guest data endpoint. Shared role endpoints
use native scoped authorization rather than one TCP port per workspace. Separate
container generations may overlap during
[generation handoff](../specs/services.md#generation-handoff); backend roles
retain single ownership while they transfer independently. Canonical workspace
namespaces preserve scoped database, file-remote and cache API access; Loom
clones use their outer repository identity. Logical namespaces and update colors
are not new tenant compromise boundaries. Compatible selected infrastructure is
reused or resumed at its recorded version; information and maintenance do not
implicitly start or repair it, and direnv reload does not implicitly upgrade it.
[CLI scope](../specs/cli.md#service-scope) defines workspace defaults, supported
`--host` selection and explicit host shutdown.
[`wrix service update`](../specs/cli.md#service-update-command) explicitly
activates the invoked host package's bound service image for this user's
existing deployment, including all four core roles even when idle or stopped and
the
[Provisioned Nix Pool Obligations](../specs/nix-store.md#bootstrap-protection-and-lifecycle),
independent of project launch data. It works outside repositories and needs no
redundant `--host`; an existing stopped deployment is started and activated,
while initial creation remains ordinary workspace `service start`. Successful
activation can leave Blue draining old connections; failed required backend
transitions are reported as partial/nonzero rather than a global rollback.
Authorized [Dolt backend handoff](../specs/services.md#dolt-backend-handoff) is
separate maintenance: it temporarily rejects new SQL connections, drains
existing sessions and transfers persistent data to one new owner. A drain
timeout leaves the old backend serving; this is not a guarantee of
interruption-free whole-deployment or stateful blue-green upgrades.

Sandboxes keep stable service/proxy endpoints. For egress proxy traffic, trusted
connection-aware routing switches new connections to Green while old proxy
connections finish on Blue; DNS-only switching or worker environment changes are
insufficient. A
[planned live grant transfer](../specs/proxy.md#planned-gateway-handoff)
preserves immutable permissions and current launcher ownership, without
restoring revoked grants. Blue retires only after its protected connections,
coordination and backend work drain, not after entire worker lifetimes. Cleanup
continues automatically after the update CLI exits, without another Wrix
invocation or GC; uncertain eligibility or cleanup failure cannot waive safety
guards or be reported as completed retirement. A stopped last selected gateway
remains selected for restart. Native routing/volume behavior still requires
Podman/krun and Apple proof. The controller supplies stable forwarding; its own
replacement requires explicit host-wide maintenance, not a promise of
transparent replacement. Package installation and ordinary start cannot upgrade
it implicitly.

[Celler/compiler backend handoff](../specs/services.md#cache-and-compiler-backend-handoff)
drains admitted native work before clean single-owner release/readiness. It
preserves signing/policy state, compiler outputs and the one 50G cache budget;
busy timeouts or uncertain completion abort rather than kill work. Unlike whole
SQL-session and egress-connection drain, proven completion of admitted Celler
workflows or compiler jobs/output/cache writes permits clean shutdown of idle
backend transports; clients may need to reconnect at unchanged endpoints. Brief
new-operation unavailability and partial update outcomes are explicit, not
transparent replay or a global rollback promise. Persistent backend areas
outlive colors and are not a shared writable database volume opened by both
generations.

[Beads storage](../specs/beads.md#storage) places each live Dolt database and
canonical sync worktree in managed service data outside the repository. Clients
receive only their namespace's endpoints and narrow sync-worktree view, not the
all-workspaces data tree. Migration and synchronization retain one authoritative
database and context-correct Git administration.

[Celler](../specs/cache.md#cache-visibility) supplies authenticated, signed
native HTTP substitution to sandboxes from approved-public common and
canonical-private views. Host cache flow is one-way publication: host Nix does
not consume this shared cache or trust its signing keys. Independently trusted
host upstreams/local store remain available. This prevents service-held signing
authority from becoming Wrix-managed host software trust, not runtime escapes,
host-client bugs or execution of agent-modified workspace content. Physical
archive deduplication is not read authority. Clients import into their own Nix
stores; Celler is neither a host-store export nor a shared installed worker
store. Host publishing/issuer files stay outside service mounts, while
cache-signing state is deliberately delegated to Celler inside the service.

The [canonical host pipeline](../specs/cache.md#canonical-host-build-pipeline)
coordinates actually selected Wrix-managed Nix realization. Actual devshell
activation and admitted host pre-push builds reconcile eligible realized
outputs, including substituted/build-only artifacts, without a second project
root list or broad store scan. [Cache policy](../specs/cache.md#cache-policy)
and verified public classification remain authoritative. Native age eviction
makes cache copies an optimization, not a permanently retained archive; real Nix
roots protect installed tools, runtime needs and in-flight publication
separately.

[Managed tool-cache modes](../specs/profiles.md#shared-and-private-tool-cache-modes)
keep personal-host Cargo/sccache/uv data separate from sandbox data. Shared mode
accepts worker-to-worker cached-data/artifact trust across repositories; private
mode uses fresh writable local tool caches. Shared Rust compilation delegates to
the service daemon, including procedural macros, rather than running several
daemons on one raw cache directory. Compiler execution belongs to the shared
service trust/resource boundary; restricted Rust requires private/local mode.
Tool-sharing controls do not disable Nix/Celler integration.

The launcher mounts only managed Cargo data and uv cache directories beneath
`/mnt/wrix/tool-caches`, linking the documented guest locations to those live
mounts on both platforms. Cargo homes remain launch-local: only registry/git,
`.package-cache`, `.package-cache-mutate` and `.global-cache` are linked. SQLite
therefore tracks the same data that the native package-cache locks protect;
configuration, credentials, installed binaries and targets are not pooled. The
native coordination fixture exercises concurrent registry/Git builds and SQLite
integrity, plus concurrent uv installs and offline cache reuse. This retains
upstream Cargo mixed-version/garbage-collection limitations: older Cargo
versions may not participate in newer tracking or mutation locking. Wrix does
not add an independent collector or promise mixed-version GC compatibility.
Native filesystem tests are not evidence of cross-VM VirtioFS lock support; the
runtime-specific system suites own that proof. Generic Darwin directory staging
still copies ordinary consumer mounts, not these live managed caches.

Full worker runtimes remain in Podman/optional-krun/Apple boundaries.
[Service-Owned Nix Stores](../specs/nix-store.md) defines separately admitted
installed-store reuse: canonical-repository pools, untrusted service builds,
read-only worker views and protected live roots. Private mode remains the
constructor/default contract; shared availability requires independent native
runtime acceptance. This neither migrates workers nor expands host Nix trust.
Sharing groups and sandbox-origin Celler publication remain outside these
contracts. Service-side compiler and Nix execution are explicit broader
capabilities, not worker-equivalent isolation.

## Agent Runtimes

The `agent` parameter selects, **at build time**, the single agent binary the
image bakes and the entrypoint execs after staging `/workspace`, settings, and
SSH credentials — selection is by build target, not by caller env. A human picks
an agent in `mkSandbox { agent = …; }`. The selected agent is encoded in the
immutable `ProfileConfig` JSON. `WRIX_AGENT` remains only the internal
build→entrypoint wire set by the Rust launcher from that config. Orchestrators
supply a matching per-call `ProfileConfig` and image.

| Value            | Behaviour                                                                                                                 |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------- |
| `direct`         | Requires an explicit consumer `agentPkg` with a nonempty, single-component `meta.mainProgram`; no built-in direct runner. |
| `claude`         | Interactive `claude` TTY, or `claude --print --input-format stream-json` when `WRIX_STDIO=1`                              |
| `pi` _(default)_ | Interactive `pi` TTY, or `pi --mode rpc` for JSONL RPC on stdio. Uses the packaged Pi coding agent.                       |

Wrix automatically adds only the selected agent runtime; consumer dependency
closures and profile additions remain intact. Pi is installed in the unsuffixed
`packages.image-<profile>` family. Explicit Claude and Pi image variants remain
available; MCP variants use constructor data rather than configured runnable
launchers. `packages.default` is the host Wrix CLI. The image declares its baked
variant in `/etc/wrix/image-agent`; before exec, the entrypoint rejects a
`ProfileConfig`/image mismatch, then verifies the selected executable is
available and fails loudly when it is absent from the image, rather than
emitting a bare `command not found`. When the internal `WRIX_AGENT` wire is
absent, dispatch uses the image-declared variant.

### Pi settings and tools

Pi images seed native `~/.pi/agent/settings.json`; consumer `agentSettings`
recursively overrides specified values while retaining unspecified defaults. The
shipped provider, model, reasoning, and display values are preferences, not
fixed sandbox contracts. Project trust defaults to `always` inside the sandbox,
and install telemetry is disabled independently of update checking.

`defaultTools = [ "+codemode" ]` enables Pi's built-in codemode alongside direct
read, Bash, edit, and write tools, even without MCP servers.
`codemode.mode = "on"` keeps those tools declared to the model. Pi owns script
execution and tool composition; Wrix adds no interpreter or runtime/profile API.
Consumers can use native settings to change tool selection or presentation, for
example:

```nix
agentSettings = {
  defaultTools = [ "-codemode" ]; # Keep direct built-ins, disable codemode.
  tuiMode = "fullscreen";
};
```

Pi sessions use the explicit `/workspace/.pi/agent/sessions` location. Durable
credential storage and live refresh follow
[Security Credential Surfaces](../specs/security.md#agent-api-credentials),
separate from the sandbox's other Pi configuration. Wrix neither imports nor
copies back the whole host or workspace Pi home. Managed credential delivery is
not an ordinary launch-time directory snapshot.

### Direct mode (orchestrator integration)

`mkSandbox { agent = "direct"; agentPkg = ...; }` is the integration seam for
external orchestrators. The consumer provides a Linux package with an explicit
`meta.mainProgram` and owns its stdio protocol. The image bakes
`${agentPkg}/bin/${agentPkg.meta.mainProgram}` into
`/etc/wrix/direct-executable`; both entrypoints run that absolute path, not a
caller-selected executable or a same-named PATH shim. Agent arguments, stdin,
stdout, stderr, and exit status reach the consumer runner unchanged. Direct
accepts omitted or empty `agentSettings`, but rejects nonempty settings. Wrix
ships no placeholder or built-in direct image family; consumers also create
matching manifests with `mkProfileImages`. See
[Loom's flake](https://github.com/taheris/loom) for the canonical wiring.

## Security Model

**Ordinary worker boundary**: Host filesystem access is limited to the selected
workspace, explicit caller mounts and scoped managed runtime views. Process/user
namespace behavior, startup capability drop and local-network protection follow
[Sandbox Isolation](../specs/sandbox.md#platform-implementations). This is not a
claim that caller mounts or workspace contents are sanitized.

**Egress**: `open` permits public internet under the protected-address baseline;
`limit` requires authenticated HTTP/CONNECT admission and blocks direct bypass.
[Proxy](../specs/proxy.md#proxy-engine) embeds Rama for HTTP forwarding and
opaque CONNECT; Wrix owns authentication, destination admission and per-launch
revocation, including active streams and connection reuse. This is requested
host/port admission, not TLS content identity or prevention of exfiltration
through permitted services.

**Delegated compiler capability**: Eligible shared compiler/procedural-macro
code executes beside Dolt, Celler and the proxy. It can affect service-visible
private data, signing authority and sandbox-cache integrity without escaping its
worker. Host Nix does not consume the shared cache or trust those signing keys;
this excludes cache-based promotion into host software trust, not runtime
escapes, host-client vulnerabilities or running agent-modified workspace
content. Ordinary API audience checks and private tool mode do not isolate that
service state from code already executing there. Host-private issuer/control
files remain unmounted; runtime authorization used by the service can still be
observed after compromise.
[Security](../specs/security.md#shared-service-trust-boundary) owns this
explicitly accepted boundary and its restricted-Rust limitations.

Repository Git policy is trusted, mutable launch input, not an agent-resistant
authorization store. Workspace writers can edit `wrix.toml` to change inherited
grants on later launches; explicit invocation overrides still win. Grants govern
only Wrix-managed key delivery. Absence of these keys does not imply a read-only
workspace, no network access, or absence of separately delivered provider
credentials. Container Git applies the effective sign grant through an
execution-local override, leaving shared repository Git config and concurrent
host signing policy unchanged.

### Execution Evidence

The host launcher establishes a secret-free execution index in `.wrix/log/`
before services or container setup, then atomically adds observed completion to
the original record. Execution, optional agent conversation, and notification
focus identities are separate. Status fields describe the foreground runtime
command, not an independently observed agent process. A completed record can
represent success or failure; an incomplete record means completion is unknown,
not that the container is running or stopped. Wrix does not poll, automatically
recover work, reconcile records in a daemon, or infer conversation IDs from
shared history. Agent transcripts and codemode summaries are non-exhaustive:
missing content does not prove an action did not occur, and intermediate tool
results or side effects may be absent. These artifacts do not provide
adversarial-agent detection or a power-loss durability guarantee.

### MicroVM Boundary (Linux)

On Linux with KVM, containers can optionally run inside a
[libkrun](https://github.com/containers/libkrun) microVM
(`podman --runtime krun`) for hardware-level isolation. Set `WRIX_MICROVM=1` to
opt in.

See [`specs/security.md`](../specs/security.md) for the full threat model.

## MCP Integration

MCP servers extend sandbox capabilities. The `mcp` parameter in `mkSandbox`
accepts a set of server names:

```nix
mkSandbox {
  profile = profiles.rust;
  mcp.playwright = { };
}
```

`mcpRuntime = true` bundles every registered server into the image and lets
`WRIX_MCP=<csv>` pick at container start: unset or `all` selects all, an
explicit empty value selects none, and unknown names fail startup. Explicit
`mcp` configuration selects its declared servers independently of this runtime
variable.

Wrix publishes the selected stdio servers in the schema-v1 `WRIX_MCP_MANIFEST`,
preserving each server's name, command, arguments, and environment. Claude
receives native `mcpServers`; Pi receives a regenerated, container-local
`~/.pi/agent/mcp.json` containing only the current selection. Wrix never copies
that generated configuration back to the host. Trusted project `.pi/mcp.json`
retains Pi's native precedence, including same-name overrides; `WRIX_MCP`
controls Wrix-managed entries, not project capabilities. External direct runners
receive the manifest unchanged and own its consumption.

Pi owns transport, discovery, namespacing, cancellation, result presentation,
and stdio-server process-tree shutdown. Wrix-managed tools use native `codemode`
exposure and names such as `mcp__playwright__browser_snapshot`, without
unqualified aliases or a Wrix protocol client. Scripts receive complete MCP
results (`content`, `structuredContent`, and `isError`), including large results
and image blocks; Pi owns model-facing truncation and image forwarding.

See [sandbox.md](../specs/sandbox.md) for selection and translation ownership,
[playwright-mcp.md](../specs/playwright-mcp.md) for browser capabilities, and
[native tmux guidance](tmux.md) for terminal debugging. The base profile
includes upstream tmux; agents can read `/etc/wrix/tmux.md` without a source
checkout. Callers select a private socket and own session cleanup; no MCP
server, pane registry, or automatic conversation cleanup is involved.

## State Layout

Workspace-local `.wrix/` holds only workspace-visible, non-secret runtime
artefacts:

```text
.wrix/
├── log/             # One host-owned JSON record per execution attempt
├── push-verified    # Single-use approval for the exact verified push transaction
└── dolt.sock        # Linux Dolt socket when used
```

Execution records are not conversations or terminal focus targets. The host
creates an incomplete record before services/container setup and atomically
completes that same file only for an observed outcome. Abrupt host-launcher
termination can leave the original record incomplete permanently. Optional
conversation identity stays null without attributable runtime reporting;
`focus_target` is only an opaque notification-routing target.

Agent transcripts live separately: Claude under `.claude/`, Pi under
`.pi/agent/sessions/`, and direct runners wherever their consumer persists them
(or nowhere). Known metadata session roots use container-absolute paths:
`/workspace/...` maps to the selected host workspace. Neither transcripts nor
operator-enabled component diagnostics are aggregated into `.wrix/log/`.
Diagnostics remain explicit and default-off; their component owns the contract,
and the enabling operator owns destination access, retention, and deletion.

[Services State Layout](../specs/services.md#state-layout) separates
host-private management/issuer/publishing state from narrowly mounted
service-visible data. [Cache State](../specs/cache.md#state-layout) owns
publication/pending manifests, credential metadata and Celler signing/archive
placement; [Beads Storage](../specs/beads.md#storage) owns the managed
database/sync worktree. These live outside repositories. Workspace-local
pointers and recorded metadata are not management authority or proof of live
readiness.

[Tool-cache storage](../specs/profiles.md#tool-cache-data-boundary) is
independent of personal-host data and the selected private/shared Nix store.
Ordinary Darwin caller mounts retain their copy semantics; managed
credential/cache delivery must satisfy its own live-update contracts. External
orchestrators such as Loom may keep their own state under `.wrix/<name>/`; Wrix
does not manage that.
