# Architecture

Wrix is a secure sandbox for running AI coding agents in isolated containers. It
provides container isolation on Linux (Podman) and macOS (Apple container CLI),
with built-in support for Pi and [Claude Code](https://claude.ai/code) (from
nixpkgs) and a direct agent slot for consumer-supplied runners, including
external orchestrators such as [Loom](https://github.com/taheris/loom), plus
tooling for notifications, remote Nix builds, and integration hooks.

## Design Principles

1. **Container isolation is the security boundary** — Filesystem and process
   isolation protect the host
2. **Least privilege** — Containers run without elevated capabilities after
   startup-time setup
3. **User namespace mapping** — Files created in `/workspace` have correct host
   ownership
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
├── wrix-cli/            # Human-facing `wrix` binary: run, spawn, service, beads
├── wrix-sandbox/        # Rust host orchestration for container launch
├── wrix-service/        # Per-workspace service lifecycle and endpoint metadata
├── wrix-cache/          # Project-cache library plus publisher/hook/server helper binaries
└── wrix-beads/          # `wrix beads push` workflow

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
├── services/            # Per-workspace <repo>-service lifecycle, Dolt, and project Nix cache
├── mcp/                 # MCP server registry
│   ├── default.nix      # Server registry: { tmux, playwright }
│   ├── tmux/            # tmux MCP server
│   └── playwright/      # Playwright MCP server
├── prek/                # Pre-commit hook shims
├── builder/             # macOS-side CLI for the Linux remote builder
├── notify/              # Desktop notifications
└── util/                # Shared utilities (container CLI shim, SSH, paths, …)

docs/
├── README.md            # Project overview, terminology
├── architecture.md      # This file
├── spec-conventions.md  # Spec-authoring conventions
└── style-rules.md       # Code standards (SH-, NX-, DOC-, GIT-, TST-, RS-, COM-, CLI-)
```

## Component Overview

| Component            | Purpose                                                                                                                                                                      | Entry Point                                               |
| -------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------- |
| Sandbox              | Container creation and lifecycle                                                                                                                                             | `mkSandbox`, `wrix run`/`spawn`                           |
| Profiles             | Pre-configured dev environments                                                                                                                                              | `profiles.{base,rust,python}`                             |
| Agent Runtime        | Single agent binary baked into the image and exec'd by the entrypoint                                                                                                        | `mkSandbox { agent = … }` / `sandbox-<profile>[-<agent>]` |
| MCP Servers          | Optional capabilities exposed through native agent clients                                                                                                                   | `mcp.playwright`, `mcpRuntime = true`                     |
| Image Source Builder | OCI image source generation via Nix (`nix-descriptor` on Linux, `docker-archive` on Darwin)                                                                                  | `lib/sandbox/image.nix`                                   |
| Profile Config       | Immutable JSON config for the Rust launcher: image ref/source/source kind, agent, mounts, non-secret env, runtime-secret names/policies, network allowlist, service features | `ProfileConfig` generated by `mkSandbox`                  |
| Profile Manifest     | JSON map of profile → profile config/image metadata for orchestrators                                                                                                        | `packages.profile-images` (`mkProfileImages`)             |
| Workspace Services   | Per-workspace Dolt and project Nix cache service container                                                                                                                   | `wrix service ...`, `<repo>-service`                      |
| Notifications        | Desktop alerts when the agent waits                                                                                                                                          | `wrix-notify`, `wrix-notifyd`                             |
| Linux Builder        | Remote Nix builds on macOS                                                                                                                                                   | `wrix-builder`                                            |

## Rust Boundary Types

Inputs are parsed by their owning component before orchestration. `wrix-core`
owns workspace hashes, cache public keys (including decoded Ed25519 key length),
and the shared YAML reader that produces a relative Git `Branch` for beads and
service planning. Service plans retain that branch and represent Dolt endpoints
as Unix/TCP payload variants, with nonzero TCP ports.

The sandbox converts complete raw JSON DTOs into launch configuration, keeping
intentional extension fields compatible. Image sources pair a nonempty path with
its format; installer construction selects only supported runtime/source
combinations. Image-store APIs carry `Digest`, `ImageRef`, `ImageId`, and typed
lookup targets through preflight, retagging, and retention. Text rendering stays
at subprocess, logging, and serialization boundaries. Filesystem availability,
container state, and network readiness remain runtime checks.

## Sandbox Launcher

The launcher and the OCI image source are separate Nix outputs, composed at the
consumer's discretion:

| Output                              | Role                                                                                                                                                                                                                                                                        |
| ----------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `packages.wrix`                     | Profile-agnostic Rust CLI; host-side `run`/`spawn`/`service`/`beads` orchestration                                                                                                                                                                                          |
| `packages.image-<profile>`          | Per-profile OCI image source built with the default `agent = "pi"` (Linux `nix-descriptor`, Darwin `docker-archive`)                                                                                                                                                        |
| `packages.image-<profile>-claude`   | Per-profile OCI image source built with `agent = "claude"` and the same platform source-kind rules                                                                                                                                                                          |
| `packages.image-<profile>-pi`       | Per-profile OCI image source built with `agent = "pi"` and the same platform source-kind rules                                                                                                                                                                              |
| `packages.sandbox-<profile>`        | Configured Pi sandbox package with explicit `bin/wrix` plus `wrix-run` as `meta.mainProgram` — the user-facing `nix run .#sandbox-rust` target                                                                                                                              |
| `packages.sandbox-<profile>-claude` | Claude overlay with agent selection encoded in `ProfileConfig` and `wrix-run` as the runnable main program                                                                                                                                                                  |
| `packages.sandbox-<profile>-pi`     | Pi overlay with agent selection encoded in `ProfileConfig` and `wrix-run` as the runnable main program. Pi images seed non-secret Codex subscription defaults; the launcher mounts Pi `auth.json` only for Pi runs. `packages.default` points at `packages.sandbox-rust-pi` |
| `packages.profile-images`           | Built-in Pi manifest mapping profile → Pi → matching raw launcher, profile config, and image metadata                                                                                                                                                                       |

The launcher exposes two subcommands sharing the same Rust-owned container
construction (mounts, env passthrough, independent Git key grants, service
startup, network firewall policy):

- `wrix run [DIR] [AGENT_ARGS…]` — interactive (TTY). Reads immutable
  image/profile/agent defaults from `ProfileConfig` JSON and runtime inputs from
  CLI/env.
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

See [specs/sandbox.md](../specs/sandbox.md) and
[specs/profiles.md](../specs/profiles.md) for the full launcher and manifest
contracts.

## Agent Runtimes

The `agent` parameter selects, **at build time**, the single agent binary the
image bakes and the entrypoint execs after staging `/workspace`, settings, and
SSH credentials — selection is by build target, not by caller env. A human picks
an agent by choosing the `mkSandbox { agent = …; }` build / its
`sandbox-<profile>[-<agent>]` target. The selected agent is encoded in the
immutable `ProfileConfig` JSON. `WRIX_AGENT` remains only the internal
build→entrypoint wire set by the Rust launcher from that config. Orchestrators
supply a matching per-call `ProfileConfig` and image.

| Value            | Behaviour                                                                                                                 |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------- |
| `direct`         | Requires an explicit consumer `agentPkg` with a nonempty, single-component `meta.mainProgram`; no built-in direct runner. |
| `claude`         | Interactive `claude` TTY, or `claude --print --input-format stream-json` when `WRIX_STDIO=1`                              |
| `pi` _(default)_ | Interactive `pi` TTY, or `pi --mode rpc` for JSONL RPC on stdio. Uses the packaged Pi coding agent.                       |

Wrix automatically adds only the selected agent runtime; consumer dependency
closures and profile additions remain intact. Pi is installed in
`packages.image-<profile>` and the unsuffixed sandbox family. Explicit Claude
and Pi variants remain available, including their `-mcp` launchers.
`packages.default` is the Rust Pi sandbox with `wrix-run` as its main program.
The image declares its baked variant in `/etc/wrix/image-agent`; before exec,
the entrypoint rejects a `ProfileConfig`/image mismatch, then verifies the
selected executable is available and fails loudly when it is absent from the
image, rather than emitting a bare `command not found`. When the internal
`WRIX_AGENT` wire is absent, dispatch uses the image-declared variant.

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

Pi sessions use the explicit `/workspace/.pi/agent/sessions` location. Its auth
storage remains isolated in the selected native config home, with only the
launcher-selected credential file mounted. Wrix neither imports nor copies back
the whole host or workspace Pi home.

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

**Protected**: Filesystem (only `/workspace` and declared mounts are
accessible), processes (isolated), user namespace (correct UID), local-network
isolation (LAN/private/host-local/VPN egress blocked), and capabilities
(startup-only network setup capabilities are dropped before the agent runs).

**Not protected by default**: Public-internet egress in `WRIX_NETWORK=open`; use
`WRIX_NETWORK=limit` to restrict public egress to the merged allowlist.

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
command, not an independently observed agent process. An incomplete record means
completion is unknown; Wrix does not poll, recover, or infer conversation IDs
from shared history. Agent transcripts and codemode summaries are non-exhaustive
evidence, not an adversarial-agent audit or a power-loss durability guarantee.

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
[tmux.md](../specs/tmux.md) for native terminal debugging.

## State Layout

Workspace-local `.wrix/` holds only workspace-visible, non-secret runtime
artefacts:

```text
.wrix/
├── log/             # Host-owned execution metadata indexes
├── push-verified    # Touched by lib/prek/hooks/pre-push on green nix flake check
└── dolt.sock        # Linux Dolt socket when used
```

Durable service/cache state, signing keys, publish manifests, endpoint metadata,
and bulky binary-cache contents live outside the worktree under the
platform-native roots defined in `specs/services.md`. External orchestrators
(e.g. Loom) may keep their own state under `.wrix/<name>/` — wrix itself doesn't
manage that.
