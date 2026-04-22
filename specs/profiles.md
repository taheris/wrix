# Profiles System

Pre-configured development environments with language-specific toolchains.

## Problem Statement

Different projects require different toolchains. Users need:
- Ready-to-use environments for common languages
- A consistent base of essential tools
- Ability to extend profiles with additional packages
- Proper environment variable configuration for each toolchain

## Requirements

### Functional

1. **Base Profile** - Core tools included in all environments
2. **Language Profiles** - Pre-configured Rust and Python environments
3. **Profile Extension** - `deriveProfile` API to extend existing profiles
4. **Package Bundling** - Profiles specify packages to include in container image
5. **Environment Configuration** - Profiles set required environment variables
6. **Mount Specifications** - Profiles can define default mounts (e.g., cargo cache)
7. **Toolchain Configuration** - Rust profile supports `withToolchain` for project-specific versions via `rust-toolchain.toml`

### Non-Functional

1. **Curated Toolkit** - Base profile ships a ready-to-work agent container (shell, search, VCS, issue tracking, formatters) — the shared floor for city agents and direct `mkSandbox` consumers alike, not a minimal OS layer. Language profiles extend it.
2. **Reproducible** - Same profile produces same environment via Nix

## Profile Attrset Schema

A profile is a Nix attrset produced by the internal `mkProfile` helper in `lib/sandbox/profiles.nix`. Fields:

| Field | Type | Purpose |
|-------|------|---------|
| `name` | string | Profile identifier (e.g. `"base"`, `"rust"`) |
| `packages` | list of derivations | Packages baked into the container image |
| `env` | attrset of strings | Environment variables set inside the container |
| `mounts` | list of mount specs | Host → container bind mounts; each `{ source, dest, mode, optional }` |
| `networkAllowlist` | list of strings | Domains permitted when `WRAPIX_NETWORK=limit` (merged with base allowlist) |
| `enabledPlugins` | attrset | Claude Code plugins merged into `~/.claude/settings.json` (e.g. `"rust-analyzer-lsp@claude-plugins-official" = true`) |
| `hostShellHook` | shell snippet | Optional snippet that downstream devShell / ralph / city shellHooks splice in to align host-side env with container-side mounts (rust uses this to pin `RUSTC_WRAPPER` and default `SCCACHE_DIR`) |
| `writableDirs` | list of strings | Linux-only: paths where the launcher stacks a tmpfs with `U=true` so the dir is wrapix-owned — needed when a ro bind mount would otherwise force the parent to be root-owned |

`deriveProfile` merges `packages`, `mounts`, `env`, and `networkAllowlist` (packages/mounts/allowlist concatenated; env right-biased). Other fields (`name`, `enabledPlugins`, `hostShellHook`, `writableDirs`) pass through from the extensions attrset if set, otherwise inherit from the base — they are not deep-merged. Callers extending a profile with extra plugins or host hooks must compose those values themselves.

## Built-in Profiles

### Base Profile

Curated developer toolkit present in every profile. Grouped by purpose:

| Category | Packages |
|----------|----------|
| Shell + POSIX core | bash, coreutils, diffutils, findutils, gawk, gnugrep, gnused, gnutar, gzip, less, patch, rsync, tree, unzip, util-linux, whichQuiet, zip |
| File + text | fd, file, ripgrep, vim |
| Network + process | curl, iproute2, iptables, iputils, lsof, netcat, openssh, procps |
| Data | jq, yq |
| Package manager | nix |
| VCS + PRs | git, gh |
| Issue tracker | beads, beads-push, dolt, gc |
| Agent tooling | man, prek, shellcheck, tmux, treefmt (wrapped with project formatters) |

`whichQuiet` is a local `pkgs.which` wrapper that suppresses `"no X in (PATH)"` noise.

`treefmt` is the project-wide formatter wrapper (nixfmt, rustfmt, shellcheck, deadnix, statix) built via `treefmt-nix.lib.mkWrapper`. Placing it in `basePackages` ensures every consumer of `profiles.base` — direct `mkSandbox` calls **and** `mkCity`-built city containers — gets the same formatters. (The code previously added treefmt only to the flake's `packages.sandbox*` outputs, so city containers missed it. Implementation plumbing for making the wrapper available to `profiles.nix` is a separate decision.)

**Base env:** none.
**Base mounts:** none. Host `~/.claude` is intentionally NOT mounted — containers use `$PROJECT_DIR/.claude` so user-level settings stay separate from project-level settings.
**Base network allowlist:** `api.anthropic.com`, `github.com`, `ssh.github.com`, `cache.nixos.org` — always permitted regardless of profile, used only when `WRAPIX_NETWORK=limit`.

### Rust Profile

Extends base with Rust toolchain via `nix-community/fenix`. fenix provides
Rust toolchains as proper Nix derivations — no dynamic linker breakage across rebuilds —
and is the same provider downstream projects are standardizing on, so store paths
align across the host/sandbox boundary.

**Toolchain:** `fenix.packages.${system}.stable.defaultToolchain` combined with
`rust-src` and `rust-analyzer` (both separately pinned via fenix). The default
toolchain matches the rustup-equivalent set: rustc + cargo + rust-std + clippy +
rustfmt + rust-docs.

| Package | Purpose |
|---------|---------|
| fenix toolchain | cargo, rustc, clippy, rustfmt, rust-std |
| fenix rust-src | Standard library source for rust-analyzer |
| fenix rust-analyzer | LSP server (pinned independently of the channel) |
| sccache | Shared compile cache across host + sandbox |
| gcc | C compiler for linking |
| openssl | TLS library (runtime) |
| openssl.dev | TLS headers (separate Nix output) |
| pkg-config | Library discovery |
| postgresql.lib | Database client libs |

Environment:

- `CARGO_HOME=/home/wrapix/.cargo` — aligns with the registry/git mount dests below so cargo actually consults the host pre-warm. Non-mounted CARGO_HOME state (credentials.toml, config.toml, `cargo install` bins) lives on tmpfs on Linux and is ephemeral across container runs — intentional for an agent-style environment.
- `RUST_SRC_PATH=${toolchain}/lib/rustlib/src/rust/library` — rust-analyzer standard library resolution
- `LIBRARY_PATH=${pkgs.postgresql.lib}/lib` — PostgreSQL library discovery at link time
- `OPENSSL_INCLUDE_DIR=${pkgs.openssl.dev}/include` — OpenSSL headers
- `OPENSSL_LIB_DIR=${pkgs.openssl.out}/lib` — OpenSSL libraries
- `RUSTC_WRAPPER=${pkgs.sccache}/bin/sccache` — route compiler invocations through sccache
- `CARGO_BUILD_RUSTC_WRAPPER` — same, picked up by cargo directly
- `SCCACHE_DIR=/home/wrapix/.cache/sccache` — stable in-container cache path, mounted from host
- `SCCACHE_CACHE_SIZE=50G` — ceiling above sccache's 10 GiB default; the default LRU-evicts mid-build for workspace-sized Rust projects. Changing this requires `sccache --stop-server` before the server picks up the new value.
- `CARGO_INCREMENTAL=0` — sccache refuses to cache any `rustc` invocation with `-C incremental=...`, so incremental compilation and sccache are redundant; disabling incremental lets every Rust compile flow through the cache.
- `CARGO_TARGET_DIR` — intentionally **unset**; cargo's per-workspace default (`<workspace>/target`) applies. The judge asserts this invariant because pinning `CARGO_TARGET_DIR` to a shared path across workspaces defeats cargo's freshness tracking and churns builds.

Mounts (host source → literal container dest; literal dests avoid the `~`-expands-on-host-launcher gotcha):

- `~/.cargo/registry` → `/home/wrapix/.cargo/registry` (ro, optional) — pre-warm crate cache from host
- `~/.cargo/git` → `/home/wrapix/.cargo/git` (ro, optional) — pre-warm git dependency cache from host
- `~/.cache/sccache` → `/home/wrapix/.cache/sccache` (rw, optional) — shared sccache store between host and sandbox

Writable dirs (`writableDirs = [ "/home/wrapix/.cargo" ]`): on Linux, the launcher stacks a tmpfs at `/home/wrapix/.cargo` with `U=true` so the dir is wrapix-owned. Without this, podman creates the mountpoint parent as root (to host the ro registry/git binds on top) and cargo can't write `.global-cache`/`credentials.toml` there. Darwin doesn't need the fix — its entrypoint creates these dirs via `mkdir -p` as namespaced-root-mapped-to-`HOST_UID`, already wrapix-writable.

Network allowlist: `crates.io`, `static.crates.io`, `index.crates.io`

> **Darwin caveat — rw sccache is session-scoped, not cross-boundary.** Apple's
> `container` CLI only exposes host paths via VirtioFS staging; the darwin
> entrypoint then `cp -r`s staged content into the profile's destination. That
> means writes to `/home/wrapix/.cache/sccache` inside a Darwin sandbox stay in
> the container's writable layer and are discarded at exit — nothing propagates
> back to the host's `~/.cache/sccache`. Cross-boundary sccache caching is
> Linux-only today. On Darwin the mount still delivers a cold pre-warm at
> container start; repeated compiles within a single container session do hit
> the cache, but subsequent sessions start cold.

> **Why not rustup?** Rustup downloads pre-built binaries dynamically linked against
> a specific glibc in the nix store. When nixpkgs is updated and the container is
> rebuilt, the old glibc path disappears and all toolchain binaries silently break
> ("No such file or directory" — the dynamic linker is missing). fenix provides
> the same toolchains as proper Nix derivations with correct dynamic linkers.
>
> **Why fenix?** fenix supports arbitrary version selection and can read
> `rust-toolchain.toml` files via `fromToolchainFile`. Downstream projects that pin
> rust via fenix on the host get identical rustc store paths inside the sandbox
> when they align the flake input (`inputs.wrapix.inputs.fenix.follows = "fenix"`),
> which in turn lets sccache hit across the host/sandbox boundary. rust-analyzer
> is combined separately so it can track its own cadence.

### Python Profile

Extends base with Python toolchain:

| Package | Purpose |
|---------|---------|
| python3 | Python interpreter |
| uv | Fast package installer |
| ruff | Linter and formatter |
| ty | Type checker |

Environment:

- `UV_CACHE_DIR=/workspace/.uv-cache` — uv cache location inside the container

Mounts:

- `~/.cache/uv` → `~/.cache/uv` (ro, optional) — pre-warm uv cache from host

Network allowlist: `pypi.org`, `files.pythonhosted.org`

## Affected Files

| File | Role |
|------|------|
| `flake.nix` | Declares `fenix` and `treefmt-nix` flake inputs; threads `fenix` and the project treefmt wrapper to `lib/sandbox/profiles.nix` via `lib/sandbox/default.nix` |
| `lib/default.nix` | Defines `deriveProfile` (merges `packages` / `mounts` / `env` / `networkAllowlist`; passes through other profile fields) |
| `lib/sandbox/default.nix` | Imports `profiles.nix` with Linux `pkgs`, host `pkgs`, `fenix`, and the treefmt wrapper; defines internal `extendProfile` helper |
| `lib/sandbox/profiles.nix` | Defines `mkProfile`, `basePackages`, built-in `base`/`rust`/`python` profiles; builds rust toolchain via `fenixPkgs.stable.defaultToolchain` combined with `rust-src` and `rust-analyzer`; wires sccache and sccache env vars, `CARGO_INCREMENTAL=0` |
| `lib/sandbox/linux/entrypoint.sh` | Contains no rustup bootstrap logic (toolchain is baked in at image build time) |
| `lib/sandbox/darwin/entrypoint.sh` | Contains no rustup bootstrap logic |

## API

```nix
# Use built-in profile (fenix stable + rust-analyzer + rust-src)
mkSandbox { profile = profiles.rust; }

# Use project's rust-toolchain.toml (fenix requires a sha256 for purity)
mkSandbox {
  profile = profiles.rust.withToolchain {
    file = ./rust-toolchain.toml;
    sha256 = "sha256-...";
  };
}

# Extend profile with additional packages
mkSandbox {
  profile = deriveProfile profiles.rust {
    packages = [ pkgs.sqlx-cli ];
    env = { DATABASE_URL = "postgres://localhost/db"; };
  };
}

# Combine: custom toolchain + extra packages
mkSandbox {
  profile = deriveProfile (profiles.rust.withToolchain {
    file = ./rust-toolchain.toml;
    sha256 = "sha256-...";
  }) {
    packages = [ pkgs.sqlx-cli ];
  };
}
```

### Profile-Specific Configuration

Profiles may expose functions for configuration specific to their toolchain.
These live on the profile attrset itself, not on `deriveProfile`.

- `profiles.rust.withToolchain` — accepts `{ file, sha256 }` (the `file` is the
  path to `rust-toolchain.toml`; `sha256` is the hash of the downloaded components),
  returns a new profile attrset (without `withToolchain`) using
  `fenix.fromToolchainFile`. `rust-src` and `rust-analyzer` are combined in on top
  of whatever components the toolchain file declares.

Only the Rust profile has `withToolchain`. Other profiles (base, python) do not
expose profile-specific configuration functions.

## Success Criteria

- [ ] Base profile provides functional development environment
  [judge](../tests/judges/profiles.sh#test_base_profile_functional)
- [ ] Rust profile can compile and run Rust projects
  [judge](../tests/judges/profiles.sh#test_rust_profile)
- [ ] Rust profile toolchain survives nixpkgs updates (no dynamic linker breakage)
  [judge](../tests/judges/profiles.sh#test_rust_profile_rebuild_stable)
- [ ] rust-analyzer can resolve the standard library (RUST_SRC_PATH is set correctly)
  [judge](../tests/judges/profiles.sh#test_rust_analyzer_sysroot)
- [ ] `profiles.rust.withToolchain { file = ./rust-toolchain.toml; sha256 = "..."; }` produces a working profile
  [judge](../tests/judges/profiles.sh#test_rust_with_toolchain)
- [ ] Python profile can run Python scripts with dependencies
  [judge](../tests/judges/profiles.sh#test_python_profile)
- [ ] deriveProfile correctly merges packages and environment
  [judge](../tests/judges/profiles.sh#test_derive_profile_merge)
- [ ] Profiles are composable (can extend extended profiles)
  [verify:wrapix](../tests/mcp/tmux/e2e/test_profile_composition.sh)

## Out of Scope

- Language-specific project scaffolding
- IDE configuration beyond Claude Code
- Auto-detection of `rust-toolchain.toml` at runtime (must be passed explicitly via `withToolchain`)

## Downstream Integration

Projects consuming wrapix that want cross-boundary sccache hits and identical
rustc store paths in the sandbox must:

1. Use fenix on the host too.
2. Route through the same fenix revision as wrapix:

   ```nix
   inputs.wrapix.url = "...";
   inputs.fenix.url = "git+https://github.com/nix-community/fenix.git?ref=main";
   inputs.wrapix.inputs.fenix.follows = "fenix";
   ```

Without this, host and sandbox will each lock fenix to their own revision and
produce different `/nix/store/.../bin/rustc` paths — sccache entries will not
cross the boundary.

No downstream gitignore is required for the sandbox cache paths: mount dests
live under `/home/wrapix/` inside the container, not under `/workspace/`, so
nothing is materialized in the project tree.
