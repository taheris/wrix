# Wrapix

Secure sandbox for running AI coding agents in isolated containers.

- **Linux**: Podman rootless container
- **macOS**: Apple [container CLI](https://github.com/apple/container) (macOS 26+, Apple Silicon)

Provides filesystem and process isolation — code inside the container cannot access your host filesystem outside `/workspace` or affect host processes. Network access is unrestricted by design. See [docs/architecture.md](docs/architecture.md) for details.

## Quick Start

```bash
nix run github:taheris/wrapix                 # base profile, claude agent
nix run github:taheris/wrapix#sandbox-rust    # rust profile, claude agent
nix run github:taheris/wrapix#sandbox-python  # python profile, claude agent
nix run github:taheris/wrapix#sandbox-rust-pi # rust profile, pi-mono agent
```

## Agent Runtimes

The agent binary that runs inside the container is selected by `WRAPIX_AGENT`:

| Agent | Runtime | How it talks to the host |
|-------|---------|--------------------------|
| `claude` *(default)* | [Claude Code](https://claude.ai/code) | Interactive TTY, or stream-json via `WRAPIX_STDIO=1` |
| `pi` | [pi-mono](https://github.com/badlogic/pi-mono) | JSONL RPC on stdio (`pi --mode rpc`) |
| `direct` | Caller-supplied binary | JSONL stdio; see *Direct mode* below |

`packages.image-<profile>` ships both `claude` and `pi` runtimes; `WRAPIX_AGENT` picks one at container start. `direct` needs the binary baked in at image-build time (see below).

## Flake Integration

```nix
{
  inputs.wrapix.url = "github:taheris/wrapix";
  inputs.flake-parts.url = "github:hercules-ci/flake-parts";

  outputs = inputs:
    inputs.flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
      perSystem = { system, ... }:
        let
          wrapix = inputs.wrapix.legacyPackages.${system}.lib;
          sandbox = wrapix.mkSandbox {
            profile = wrapix.profiles.rust;   # or base, python
            # packages = [ linuxPkgs.sqlx-cli ];
            # env.DATABASE_URL = "postgres://localhost/mydb";
            # mcp.tmux = { };
          };
        in {
          packages.default = sandbox.package;
        };
    };
}
```

### mkSandbox Options

| Option | Type | Description |
|--------|------|-------------|
| `profile` | profile attrset | Base environment (`profiles.{base,rust,python}`) |
| `packages` | list of packages | Additional Nix packages to include |
| `env` | attrset of strings | Environment variables |
| `mounts` | list of `{ source, dest, mode }` | Host paths to mount into the container |
| `agent` | `"claude"` \| `"pi"` \| `"direct"` | Agent runtime baked into the image (default `"claude"`) |
| `directRunner` | Linux derivation | Required when `agent == "direct"`; supplies the binary the entrypoint execs |
| `deployKey` | string | SSH key name for git push (see `scripts/setup-deploy-key`) |
| `mcp` | attrset of server configs | Baked-in MCP servers (e.g. `{ tmux = { }; }`) |
| `mcpRuntime` | bool | Include all MCP servers, select at runtime via `WRAPIX_MCP` |
| `model` | string | Override `ANTHROPIC_MODEL` for this container |

See [specs/sandbox.md](specs/sandbox.md) for full details.

## Profiles

| Profile | Packages |
|---------|----------|
| `base` | git, ripgrep, fd, jq, vim, treefmt wrapper |
| `rust` | base + fenix toolchain, sccache, gcc, openssl, pkg-config |
| `python` | base + python3, uv, ty, ruff |

See [specs/profiles.md](specs/profiles.md) for the full schema and `buildPackage` API.

## Direct Mode

`agent = "direct"` is the integration seam for external orchestrators (e.g. [Loom](https://github.com/taheris/loom)) that drive the container themselves over JSONL stdio. Wrapix doesn't bundle a direct runner — the consumer hands one in:

```nix
let
  wrapix    = inputs.wrapix.legacyPackages.${system}.lib;
  loomLinux = inputs.loom.lib.mkLoom { pkgs = inputs.nixpkgs.legacyPackages.x86_64-linux; };
  sandbox   = wrapix.mkSandbox {
    profile      = wrapix.profiles.rust;
    agent        = "direct";
    directRunner = loomLinux.bin;
  };
in
{ packages.sandbox = sandbox.package; }
```

The launcher exposes two entry points (both honour the profile's mounts, env passthrough, and deploy key):

- `wrapix run [DIR] [CMD…]` — interactive TTY.
- `wrapix spawn --spawn-config <file> [--stdio]` — programmatic JSONL dispatch. The orchestrator writes a `SpawnConfig` JSON file with `image_ref`, `image_source`, `workspace`, `env`, and `agent_args`, then pipes JSONL on stdin/stdout.

## MCP Servers

```bash
nix run github:taheris/wrapix#sandbox-mcp         # base + all MCP servers
nix run github:taheris/wrapix#sandbox-rust-mcp    # rust + all MCP servers
WRAPIX_MCP=tmux nix run .#sandbox-mcp             # select specific servers
```

Available: [tmux](specs/tmux-mcp.md) (pane management for debugging), [playwright](specs/playwright-mcp.md) (browser automation). In flakes: `mcp.tmux = { }` or `mcpRuntime = true`.

## Notifications

Desktop alerts when the agent needs attention: `nix run github:taheris/wrapix#wrapix-notifyd`. See [specs/notifications.md](specs/notifications.md).

## Linux Builder (macOS)

Remote Nix builds for aarch64-linux on macOS: `wrapix-builder start && wrapix-builder setup`. See [specs/linux-builder.md](specs/linux-builder.md).
