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
| `shellHook` | shell snippet | Optional snippet for downstream **host** devShells / ralph / city shellHooks to splice in (e.g. `${rustProfile.shellHook}`). Aligns host-side toolchain identity, env, and PATH with the sandbox so `rustc` resolves to the same `/nix/store/...` path on both sides — the prerequisite for cross-boundary sccache hits and shared `target/` artifact reuse. |
| `writableDirs` | list of strings | Linux-only: paths where the launcher stacks a tmpfs with `U=true` so the dir is wrapix-owned — needed because podman creates bind-mount parents as root, which blocks writes to sibling files like `.global-cache`/`credentials.toml` |

Mount specs use `optional = true` to mean "skip this bind silently if the host source path does not exist", letting profiles declare cache mounts that no-op on hosts that haven't yet populated them.

`deriveProfile` merges `packages`, `mounts`, `env`, and `networkAllowlist` (packages/mounts/allowlist concatenated; env right-biased). Other fields (`name`, `enabledPlugins`, `shellHook`, `writableDirs`) pass through from the extensions attrset if set, otherwise inherit from the base — they are not deep-merged. Callers extending a profile with extra plugins or shell hooks must compose those values themselves.

The rust profile additionally exposes `toolchain` (the resolved fenix `combine` derivation) and `withToolchain` (a configuration function); both pass through `deriveProfile` since extensions don't override them. See [Rust Profile](#rust-profile) for details.

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

`treefmt` is the project-wide formatter wrapper (nixfmt, rustfmt, shellcheck, deadnix, statix) built via `treefmt-nix.lib.mkWrapper`. Placing it in `basePackages` ensures every consumer of `profiles.base` — direct `mkSandbox` calls **and** `mkCity`-built city containers — gets the same formatters.

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

- `CARGO_HOME=/home/wrapix/.cargo` — aligns with the registry/git mount dests below so cargo reads from and writes back to the shared host cache. Non-mounted CARGO_HOME state (credentials.toml, config.toml, `cargo install` bins) lives on tmpfs on Linux and is ephemeral across container runs — intentional for an agent-style environment.
- `RUST_SRC_PATH=${toolchain}/lib/rustlib/src/rust/library` — rust-analyzer standard library resolution
- `LIBRARY_PATH=${pkgs.postgresql.lib}/lib` — PostgreSQL library discovery at link time
- `OPENSSL_INCLUDE_DIR=${pkgs.openssl.dev}/include` — OpenSSL headers
- `OPENSSL_LIB_DIR=${pkgs.openssl.out}/lib` — OpenSSL libraries
- `RUSTC_WRAPPER=${pkgs.sccache}/bin/sccache` — route compiler invocations through sccache
- `CARGO_BUILD_RUSTC_WRAPPER=${pkgs.sccache}/bin/sccache` — same value, picked up by cargo directly
- `SCCACHE_DIR=/home/wrapix/.cache/sccache` — stable in-container cache path, mounted from host
- `SCCACHE_CACHE_SIZE=50G` — ceiling above sccache's 10 GiB default; the default LRU-evicts mid-build for workspace-sized Rust projects. Changing this requires `sccache --stop-server` before the server picks up the new value.
- `CARGO_INCREMENTAL=0` — sccache refuses to cache any `rustc` invocation with `-C incremental=...`, so incremental compilation and sccache are redundant; disabling incremental lets every Rust compile flow through the cache.
- `CARGO_TARGET_DIR` — intentionally **unset**; cargo's per-workspace default (`<workspace>/target`) applies. The judge asserts this invariant because pinning `CARGO_TARGET_DIR` to a shared path across workspaces defeats cargo's freshness tracking and churns builds.

**Host devshell alignment.** The profile's `shellHook` (spliced into downstream host devShells alongside `ralph.shellHook`) prepends `${toolchain}/bin` to `PATH` and re-exports `RUSTC_WRAPPER`, `SCCACHE_DIR`, `SCCACHE_CACHE_SIZE`, and `CARGO_INCREMENTAL=0` so the host shell uses the same fenix-pinned `rustc` binary as the sandbox. Without the PATH prepend, host PATH falls through to rustup's `rustc` (or whichever appears first), and the diverging sysroot path baked into rlib metadata invalidates every sccache key across the boundary — even when both sides report the same Rust version. `withToolchain` rebuilds the snippet over the custom toolchain so `packages`, `env`, `shellHook`, and `toolchain` (see below) all close over the same derivation.

**Toolchain derivation (`profile.toolchain`).** The rust profile exposes the resolved fenix `combine` derivation as `profile.toolchain` — the same store path that lands in `packages` and is interpolated into `shellHook`'s PATH prepend. Sibling Nix apps that run cargo (e.g. `pkgs.writeShellApplication { runtimeInputs = [ rustProfile.toolchain ]; ... }`) must point `runtimeInputs` at this field rather than re-instantiating fenix in their own flake. Re-instantiation produces a different `/nix/store/...` path even when fenix versions match, and again when calling `fromToolchainFile` directly (bare `rust-<ver>` vs the `combine`-wrapped `rust-mixed`); sccache hashes the compiler binary, so a divergent path means every cache key misses across the boundary. The default profile and the `withToolchain` variant both set this field to the toolchain they were built from.

Mounts (host source → literal container dest; literal dests avoid the `~`-expands-on-host-launcher gotcha):

- `~/.cargo/registry` → `/home/wrapix/.cargo/registry` (rw, optional) — shared crate cache between host and sandbox; pre-warms at launch and writes back as cargo downloads crates not in the pre-warm set. `ro` here breaks any cargo command that needs a fresh crate (`Read-only file system (os error 30)` writing to `registry/index/.../.cache/...` or `registry/cache/...`), since cargo's pre-fetch path is the same as its on-demand download path.
- `~/.cargo/git` → `/home/wrapix/.cargo/git` (rw, optional) — shared git dependency cache; same rw rationale as registry (cargo writes new git checkouts here on cache miss).
- `~/.cache/sccache` → `/home/wrapix/.cache/sccache` (rw, optional) — shared sccache store between host and sandbox

Writable dirs (`writableDirs = [ "/home/wrapix/.cargo" ]`): on Linux, the launcher stacks a tmpfs at `/home/wrapix/.cargo` with `U=true` so the dir is wrapix-owned. Without this, podman creates the mountpoint parent as root (to host the registry/git binds on top, regardless of mount mode) and cargo can't write `.global-cache`/`credentials.toml` there. Darwin doesn't need the fix — its entrypoint creates these dirs via `mkdir -p` as namespaced-root-mapped-to-`HOST_UID`, already wrapix-writable.

Network allowlist: `crates.io`, `static.crates.io`, `index.crates.io`

> **Darwin caveat — rw cache mounts are session-scoped, not cross-boundary.**
> Apple's `container` CLI only exposes host paths via VirtioFS staging; the darwin
> entrypoint then `cp -r`s staged content into the profile's destination. That
> means writes to any rw cache mount inside a Darwin sandbox — `/home/wrapix/.cargo/registry`,
> `/home/wrapix/.cargo/git`, `/home/wrapix/.cache/sccache`, and (per the Python profile)
> `/home/wrapix/.cache/uv` — stay in the container's writable layer and are discarded
> at exit; nothing propagates back to the corresponding `~/.cargo/{registry,git}`,
> `~/.cache/sccache`, or `~/.cache/uv`. Cross-boundary cache reuse is Linux-only today.
> On Darwin the mounts still deliver a cold pre-warm at container start; in-session
> cargo/uv downloads and compile-cache writes succeed (so the read-only-FS symptom does
> not reproduce on Darwin), but each new container session starts with only what was
> on the host at launch.

> **Why not rustup?** Rustup downloads pre-built binaries dynamically linked against
> a specific glibc in the nix store. When nixpkgs is updated and the container is
> rebuilt, the old glibc path disappears and all toolchain binaries silently break
> ("No such file or directory" — the dynamic linker is missing). fenix provides
> the same toolchains as proper Nix derivations with correct dynamic linkers.
>
> **Why fenix?** fenix supports arbitrary version selection and can read
> `rust-toolchain.toml` files via `fromToolchainFile`. Identical rustc store
> paths across the host/sandbox/sibling-app boundary — required for sccache
> hits — come from splicing `rustProfile.shellHook` into the downstream
> devShell and using `rustProfile.toolchain` in sibling app `runtimeInputs`
> (see *Downstream Integration*). rust-analyzer is combined separately so it
> can track its own cadence.

### Python Profile

Extends base with Python toolchain:

| Package | Purpose |
|---------|---------|
| python3 | Python interpreter |
| uv | Fast package installer |
| ruff | Linter and formatter |
| ty | Type checker |

Environment:

- `UV_CACHE_DIR=/home/wrapix/.cache/uv` — points at the cache mount dest below so uv reads from and writes back to the shared host cache (mirrors the rust profile's `CARGO_HOME` ↔ registry-mount alignment).

Mounts (host source → literal container dest):

- `~/.cache/uv` → `/home/wrapix/.cache/uv` (rw, optional) — shared uv cache between host and sandbox; pre-warms at launch and writes back on cache miss. `ro` here would break any `uv` invocation that needs a package not in the pre-warm set, same failure mode as the cargo registry.

Network allowlist: `pypi.org`, `files.pythonhosted.org`

## Affected Files

| File | Role |
|------|------|
| `flake.nix` | Declares `fenix` and `treefmt-nix` flake inputs; threads `fenix` and the project treefmt wrapper to `lib/sandbox/profiles.nix` via `lib/sandbox/default.nix` |
| `lib/default.nix` | Defines `deriveProfile` (merges `packages` / `mounts` / `env` / `networkAllowlist`; passes through other profile fields) |
| `lib/sandbox/default.nix` | Imports `profiles.nix` with Linux `pkgs`, host `pkgs`, `fenix`, and the treefmt wrapper; defines internal `extendProfile` helper |
| `lib/sandbox/profiles.nix` | Defines `mkProfile`, `basePackages`, built-in `base`/`rust`/`python` profiles; builds rust toolchain via `fenixPkgs.stable.defaultToolchain` combined with `rust-src` and `rust-analyzer`; wires sccache and sccache env vars, `CARGO_INCREMENTAL=0`; exposes the per-profile host `shellHook` (rust prepends `${toolchain}/bin` to host PATH and re-exports sccache env); exposes `profile.toolchain` on the rust profile (default and `withToolchain` variants) so consumer-side derivations can share the sandbox's toolchain store path |
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

# Reuse the same toolchain derivation in a sibling Nix app so its `rustc` shares
# a /nix/store/... path with the sandbox image and the host devshell PATH.
let
  rustProfile = profiles.rust.withToolchain {
    file = ./rust-toolchain.toml;
    sha256 = "sha256-...";
  };
in
pkgs.writeShellApplication {
  name = "test-ci";
  runtimeInputs = [ rustProfile.toolchain ];
  text = "cargo nextest run";
}
```

### Profile-Specific Configuration

Profiles may expose functions for configuration specific to their toolchain.
These live on the profile attrset itself, not on `deriveProfile`.

- `profiles.rust.withToolchain` — accepts `{ file, sha256 }` (the `file` is the
  path to `rust-toolchain.toml`; `sha256` is the hash of the downloaded components),
  returns a new profile attrset (without `withToolchain`) using
  `fenix.fromToolchainFile`. `rust-src` and `rust-analyzer` are combined in on top
  of whatever components the toolchain file declares. `packages`, `env`,
  `shellHook`, and `toolchain` are all rebuilt against the custom toolchain so
  they close over the same derivation — `env.RUST_SRC_PATH` resolves to the new
  toolchain's stdlib source, the host PATH prepend in `shellHook` resolves to
  the same `rustc` binary the sandbox uses, and `profile.toolchain` is exactly
  that derivation for downstream `runtimeInputs`.

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
- [ ] Cargo registry and git mounts are writable so cargo can fetch crates not in the pre-warm set without `Read-only file system` errors
  [judge](../tests/judges/profiles.sh#test_cargo_registry_writable)
- [ ] Python profile can run Python scripts with dependencies
  [judge](../tests/judges/profiles.sh#test_python_profile)
- [ ] deriveProfile correctly merges packages and environment
  [judge](../tests/judges/profiles.sh#test_derive_profile_merge)
- [ ] Profiles are composable (can extend extended profiles)
  [verify:wrapix](../tests/mcp/tmux/e2e/test_profile_composition.sh)
- [ ] Host devshell that splices `rustProfile.shellHook` resolves `rustc` to the same `/nix/store/...` path as the sandbox
  [judge](../tests/judges/profiles.sh#test_host_sandbox_rustc_same_store_path)
- [ ] `profile.toolchain` is exposed on both the default rust profile and `withToolchain { file; sha256; }`, and points at the same derivation referenced by the profile's `packages` and the `${toolchain}/bin` PATH prepend in `shellHook`
  [judge](../tests/judges/profiles.sh#test_rust_toolchain_field)

## Out of Scope

- Language-specific project scaffolding
- IDE configuration beyond Claude Code
- Auto-detection of `rust-toolchain.toml` at runtime (must be passed explicitly via `withToolchain`)
- Automatic pruning of cargo registry / git / uv caches — operators are expected to clean `~/.cargo/{registry,git}` and `~/.cache/uv` manually if they grow unbounded; sccache is self-capped via `SCCACHE_CACHE_SIZE`.

## Downstream Integration

Cross-boundary sccache hits require the host shell, sandbox image, and any
sibling Nix app derivations that run cargo to all reference the same toolchain
derivation. Wrapix exposes two surfaces for this — splice `rustProfile.shellHook`
into the host devShell, and use `rustProfile.toolchain` in `runtimeInputs` of
any sibling derivation that runs cargo. Both close over the same `/nix/store/...`
path the sandbox bakes in.

For the host devshell, splice the rust profile's `shellHook` parallel to
`ralph.shellHook`:

```nix
let
  rustProfile = wrapix.profiles.rust.withToolchain {
    file = ./rust-toolchain.toml;
    sha256 = "sha256-...";
  };
  sandbox = wrapix.mkSandbox { profile = rustProfile; };
  ralph = wrapix.mkRalph { inherit sandbox; };
in pkgs.mkShell {
  shellHook = ''
    ${ralph.shellHook}
    ${rustProfile.shellHook}
  '';
}
```

The hook prepends the profile's pinned toolchain to `PATH`, so host `rustc`
resolves to the same `/nix/store/.../bin/rustc` the sandbox uses. This is the
prerequisite for cross-boundary sccache hits and shared `target/` artifact
reuse: when host and sandbox compile against the same toolchain derivation,
sysroot paths in rlib metadata match, sccache keys collide, and cargo's
fingerprints in `/workspace/target/` stay valid across switches.

**Sibling Nix apps that run cargo.** Aligning the host shell PATH is not
enough on its own — any sibling app derivation in the consumer's flake
(`pkgs.writeShellApplication`, a `nix run` target, a CI runner) ships its own
`runtimeInputs` and bypasses PATH from the devshell. If that derivation
re-instantiates fenix (`inputs.fenix.packages.fromToolchainFile { ... }`), it
gets a different `/nix/store/...` path than the sandbox/host even when fenix
versions match, and an additional difference between the bare `rust-<ver>`
output and the `combine`-wrapped `rust-mixed` the profile builds. sccache
hashes the compiler binary, so cache keys never collide and an 11 GiB host
cache populated from inside the sandbox can produce 0 hits when invoked from
the sibling app.

The fix is to point `runtimeInputs` at `rustProfile.toolchain`, which is the
exact derivation the profile baked into the image and prepended to PATH:

```nix
packages.test-ci = pkgs.writeShellApplication {
  name = "test-ci";
  runtimeInputs = [ rustProfile.toolchain ];
  text = "cargo nextest run";
};
```

Consumers with their own `rust-toolchain.toml` feed it to `withToolchain`
once; `rustProfile.toolchain` is then the `combine`-wrapped output of THEIR
file, identical across sandbox image, host shellHook PATH, and sibling app
`runtimeInputs`. They never need to call fenix directly.

**Escape hatch.** If a downstream wants to control `rustc` independently of
wrapix's pin (e.g., to use a different fenix revision on the host), it can
skip the splice and instead lock its fenix flake input to wrapix's:

```nix
inputs.wrapix.url = "...";
inputs.fenix.url = "git+https://github.com/nix-community/fenix.git?ref=main";
inputs.wrapix.inputs.fenix.follows = "fenix";
```

Without aligning toolchain identity through one of these surfaces (shellHook
splice for the devshell, `profile.toolchain` for sibling apps, or the `follows`
escape hatch), host, sandbox, and sibling derivations lock fenix to their own
revisions, produce different `/nix/store/.../bin/rustc` paths, and sccache
entries do not cross the boundary.

No downstream gitignore is required for the sandbox cache paths: mount dests
live under `/home/wrapix/` inside the container, not under `/workspace/`, so
nothing is materialized in the project tree.
