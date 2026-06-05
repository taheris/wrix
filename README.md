# Wrapix

Secure sandbox for running AI coding agents in isolated containers.

- **Linux**: Podman rootless container
- **macOS**: Apple [container CLI](https://github.com/apple/container) (macOS 26+, Apple Silicon)

Provides filesystem and process isolation — code inside the container cannot access your host filesystem outside `/workspace` or affect host processes. Network access is unrestricted by design. See [docs/architecture.md](docs/architecture.md) for details.

## Quick Start

```bash
nix run github:taheris/wrapix                      # base profile, pi agent
nix run github:taheris/wrapix#sandbox-rust         # rust profile, direct base image
nix run github:taheris/wrapix#sandbox-rust-pi      # rust profile, pi agent overlay
nix run github:taheris/wrapix#sandbox-rust-claude  # rust profile, claude overlay
```

## Agent Runtimes

The agent binary baked into the image is selected **at build time** by `mkSandbox { agent = …; }`, surfaced as the `sandbox-<profile>[-<agent>]` flake output. Exactly one agent rides each image.

| Agent | Runtime | How it talks to the host |
|-------|---------|--------------------------|
| `direct` *(default)* | Direct runner binary | JSONL stdio; intended for orchestrators |
| `claude` | [Claude Code](https://claude.ai/code) | Interactive TTY, or stream-json via `WRAPIX_STDIO=1` |
| `pi` | [Pi coding agent](https://github.com/earendil-works/pi) | Interactive TTY, or JSONL RPC on stdio (`pi --mode rpc`) |

`packages.image-<profile>` ships the default direct runtime. Agent overlays are exposed as `packages.image-<profile>-claude` and `packages.image-<profile>-pi`.

Pi images seed OpenAI Codex subscription defaults and high reasoning. Run `/login` in Pi and choose ChatGPT Plus/Pro (Codex) once; credentials persist in host `~/.pi/agent/auth.json`. Pi session state is written under workspace `.pi/`, so consumer repositories should add `.pi/` to `.gitignore`.

## Flake Integration

The canonical pattern feeds one profile to both the host devshell and the sandbox image, so `rustc` resolves to the same `/nix/store/...` path on both sides (the prerequisite for cross-boundary sccache hits):

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
          rustProfile = wrapix.rustProfile {
            toolchain = ./rust-toolchain.toml;
            sha256    = "sha256-...";
          };
        in {
          devShells.default = wrapix.mkDevShell { profile = rustProfile; };
          packages.image    = (wrapix.mkSandbox { profile = rustProfile; }).image;
        };
    };
}
```

`wrapix.mkDevShell { profile = ...; }` is the sole entry point for profile-aware host devshells — it splices `profile.shellHook` automatically, so consumers never hand-roll PATH or `RUSTC_WRAPPER` exports. See [specs/profiles.md](specs/profiles.md) for the `rustProfile` constructor signature and `mkDevShell` composition rules.

### mkSandbox Options

| Option | Type | Description |
|--------|------|-------------|
| `profile` | profile attrset | Base environment (`profiles.{base,rust,python}`) |
| `packages` | list of packages | Additional Nix packages to include |
| `env` | attrset of strings | Environment variables |
| `mounts` | list of `{ source, dest, mode }` | Host paths to mount into the container |
| `agent` | `"direct"` \| `"claude"` \| `"pi"` | Agent runtime baked into the image (default `"direct"`) |
| `agentPkg` | Linux derivation or `null` | Optional selected-agent package override |
| `agentSettings` | attrset | Settings for the selected agent (`claude` or `pi`) |
| `deployKey` | string | SSH key name for git push (see `scripts/setup-deploy-key`) |
| `mcp` | attrset of server configs | Baked-in MCP servers (e.g. `{ tmux = { }; }`) |
| `mcpRuntime` | bool | Include all MCP servers, select at runtime via `WRAPIX_MCP` |

See [specs/sandbox.md](specs/sandbox.md) for full details.

## Profiles

| Profile | Packages |
|---------|----------|
| `base` | git, ripgrep, fd, jq, vim, treefmt wrapper |
| `rust` | base + fenix toolchain, sccache, gcc, openssl, pkg-config |
| `python` | base + python3, uv, ty, ruff |

See [specs/profiles.md](specs/profiles.md) for the full schema and `buildPackage` API.

## Consumer-supplied agents

`agent = "direct"` is the integration seam for external orchestrators (e.g. [Loom](https://github.com/taheris/loom)) that drive the container themselves over JSONL stdio. The built-in direct image carries a placeholder runner so the default image family is buildable; production orchestrators should pass their own `agentPkg`:

```nix
let
  wrapix    = inputs.wrapix.legacyPackages.${system}.lib;
  loomLinux = inputs.loom.lib.mkLoom { pkgs = inputs.nixpkgs.legacyPackages.x86_64-linux; };
  sandbox   = wrapix.mkSandbox {
    profile  = wrapix.profiles.rust;
    agent    = "direct";
    agentPkg = loomLinux.bin;
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
