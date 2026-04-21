# Wrapix

Secure sandbox for running [Claude Code](https://claude.ai/code) in isolated containers.

- **Linux**: Podman rootless container
- **macOS**: Apple [container CLI](https://github.com/apple/container) (macOS 26+, Apple Silicon)

Provides filesystem and process isolation — code inside the container cannot access your host filesystem outside `/workspace` or affect host processes. Network access is unrestricted by design. See [docs/architecture.md](docs/architecture.md) for details.

## Quick Start

```bash
nix run github:taheris/wrapix                # base profile
nix run github:taheris/wrapix#wrapix-rust    # rust profile
nix run github:taheris/wrapix#wrapix-python  # python profile
```

## Flake Integration

### Sandbox only

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

### With Gas City (multi-agent orchestration)

```nix
perSystem = { system, ... }:
  let
    wrapix = inputs.wrapix.legacyPackages.${system}.lib;
    city = wrapix.mkCity {
      profile = wrapix.profiles.rust;
      services.api.package = myApp;
      secrets.claude = "ANTHROPIC_API_KEY";
    };
  in {
    packages.default = city.sandbox.package;  # ad-hoc container
    apps.city = city.app;                      # gc start --foreground
    apps.ralph = city.ralph.app;               # ralph in a sandbox
    devShells.default = city.devShell;         # gc, bd, ralph — all on PATH
  };
```

`mkCity` creates one shared sandbox and threads it through everything. The devShell gives you `gc`, `bd`, `ralph`, and `wrapix` — all ready to use. Use `city.mkDevShell { packages = [ ... ]; }` to add your own packages.

## mkSandbox Options

| Option | Type | Description |
|--------|------|-------------|
| `profile` | profile attrset | Base environment (`profiles.base`, `.rust`, `.python`) |
| `packages` | list of packages | Additional Nix packages to include |
| `env` | attrset of strings | Environment variables |
| `mounts` | list of `{ source, dest, mode }` | Host paths to mount into the container |
| `deployKey` | string | SSH key name for git push (see `scripts/setup-deploy-key`) |
| `mcp` | attrset of server configs | Baked-in MCP servers (e.g. `{ tmux = { }; }`) |
| `mcpRuntime` | bool | Include all MCP servers, select at runtime via `WRAPIX_MCP` |

See [specs/sandbox.md](specs/sandbox.md) for full details.

## mkCity Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `profile` | profile or string | `"base"` | Shared sandbox profile |
| `sandbox` | sandbox attrset | — | Use existing sandbox instead of profile |
| `services` | attrset | `{ }` | Service containers to monitor |
| `secrets` | attrset of strings | `{ }` | Secrets (`"/path"` = file, else = env var) |
| `name` | string | `"dev"` | City name (used for container/network naming) |
| `workers` | int | `1` | Max concurrent workers |
| `cooldown` | string | `"0"` | Delay between task dispatches |
| `scout` | `{ interval, maxBeads }` | `5m`, `10` | Scout polling config |
| `resources` | attrset | `{ }` | Per-role resource limits |

See [specs/gas-city.md](specs/gas-city.md) for the full specification.

## Profiles

| Profile | Packages |
|---------|----------|
| `base` | git, ripgrep, fd, jq, vim |
| `rust` | base + fenix toolchain, sccache, gcc, openssl, pkg-config |
| `python` | base + python3, uv, ty, ruff |

## NixOS Module

```nix
services.wrapix.cities.myproject = {
  workspace = "/srv/myproject";
  profile = "rust";
  services.api.package = myApp;
  secrets.claude = "/run/secrets/claude-api-key";
};
```

Generates systemd units and a podman network per city.

## MCP Servers

```bash
nix run github:taheris/wrapix#wrapix-mcp         # base + all MCP servers
nix run github:taheris/wrapix#wrapix-rust-mcp    # rust + all MCP servers
WRAPIX_MCP=tmux nix run .#wrapix-mcp             # select specific servers
```

Available: [tmux](specs/tmux-mcp.md) (pane management for debugging), [playwright](specs/playwright-mcp.md) (browser automation). In flakes: `mcp.tmux = { }` or `mcpRuntime = true`.

## Notifications

Desktop alerts when Claude needs attention: `nix run github:taheris/wrapix#wrapix-notifyd`. See [specs/notifications.md](specs/notifications.md).

## Linux Builder (macOS)

Remote Nix builds for aarch64-linux on macOS: `wrapix-builder start && wrapix-builder setup`. See [specs/linux-builder.md](specs/linux-builder.md).
