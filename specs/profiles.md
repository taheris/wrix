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
8. **Rust Package Construction** - Rust profile exposes `buildPackage` for crane-backed Rust packages with split `bin`/`clippy`/`nextest` derivations

### Non-Functional

1. **Curated Toolkit** - Base profile ships a ready-to-work agent container (shell, search, VCS, issue tracking, formatters) for `mkSandbox` consumers — the shared floor, not a minimal OS layer. Language profiles extend it.
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
| `shellHook` | shell snippet | Optional snippet for downstream **host** devShells to splice in (e.g. `${rustProfile.shellHook}`). Aligns host-side toolchain identity, env, and PATH with the sandbox so `rustc` resolves to the same `/nix/store/...` path on both sides — the prerequisite for cross-boundary sccache hits and shared `target/` artifact reuse. |
| `writableDirs` | list of strings | Linux-only: paths where the launcher stacks a tmpfs with `U=true` so the dir is wrapix-owned — needed because podman creates bind-mount parents as root, which blocks writes to sibling files like `.global-cache`/`credentials.toml` |

Mount specs use `optional = true` to mean "skip this bind silently if the host source path does not exist", letting profiles declare cache mounts that no-op on hosts that haven't yet populated them.

`deriveProfile` merges `packages`, `mounts`, `env`, and `networkAllowlist` (packages/mounts/allowlist concatenated; env right-biased). Other fields (`name`, `enabledPlugins`, `shellHook`, `writableDirs`) pass through from the extensions attrset if set, otherwise inherit from the base — they are not deep-merged. Callers extending a profile with extra plugins or shell hooks must compose those values themselves.

The rust profile additionally exposes `toolchain` (the resolved fenix `combine` derivation), `withToolchain` (a configuration function), and `buildPackage` (a crane-backed Rust package builder); all three pass through `deriveProfile` since extensions don't override them. See [Rust Profile](#rust-profile) for details.

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

`treefmt` is the project-wide formatter wrapper (nixfmt, rustfmt, shellcheck, deadnix, statix) built via `treefmt-nix.lib.mkWrapper`. Placing it in `basePackages` ensures every consumer of `profiles.base` gets the same formatters.

**Base env:** none.
**Base mounts:** none. Host `~/.claude` is intentionally NOT mounted — containers use `$PROJECT_DIR/.claude` so user-level settings stay separate from project-level settings.
**Base network allowlist:** `api.anthropic.com`, `github.com`, `ssh.github.com`, `cache.nixos.org` — always permitted regardless of profile, used only when `WRAPIX_NETWORK=limit`.

### Rust Profile

Extends base with Rust toolchain via `nix-community/fenix`. fenix provides
Rust toolchains as proper Nix derivations — no dynamic linker breakage across rebuilds —
and is the same provider downstream projects are standardizing on, so store paths
align across the host/sandbox boundary.

**Toolchain:** `fenix.packages.${system}.stable.defaultToolchain` combined with
`rust-src` (separately pinned) and `stable.rust-analyzer-preview` (manifest build
from the stable channel — see *Why stable rust-analyzer?* below). The default
toolchain matches the rustup-equivalent set: rustc + cargo + rust-std + clippy +
rustfmt + rust-docs.

| Package | Purpose |
|---------|---------|
| fenix toolchain | cargo, rustc, clippy, rustfmt, rust-std |
| fenix rust-src | Standard library source for rust-analyzer |
| fenix `stable.rust-analyzer-preview` | LSP server (manifest build, channel-aligned with stable) |
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

**Host devshell alignment.** The profile's `shellHook` (spliced into downstream host devShells) prepends `${toolchain}/bin` to `PATH` and re-exports `RUSTC_WRAPPER`, `SCCACHE_DIR`, `SCCACHE_CACHE_SIZE`, and `CARGO_INCREMENTAL=0` so the host shell uses the same fenix-pinned `rustc` binary as the sandbox. Without the PATH prepend, host PATH falls through to rustup's `rustc` (or whichever appears first), and the diverging sysroot path baked into rlib metadata invalidates every sccache key across the boundary — even when both sides report the same Rust version. `withToolchain` rebuilds the snippet over the custom toolchain so `packages`, `env`, `shellHook`, and `toolchain` (see below) all close over the same derivation.

**Toolchain derivation (`profile.toolchain`).** The rust profile exposes the resolved fenix `combine` derivation as `profile.toolchain` — the same store path that lands in `packages` and is interpolated into `shellHook`'s PATH prepend. Sibling Nix apps that run cargo (e.g. `pkgs.writeShellApplication { runtimeInputs = [ rustProfile.toolchain ]; ... }`) must point `runtimeInputs` at this field rather than re-instantiating fenix in their own flake. Re-instantiation produces a different `/nix/store/...` path even when fenix versions match, and again when calling `fromToolchainFile` directly (bare `rust-<ver>` vs the `combine`-wrapped `rust-mixed`); sccache hashes the compiler binary, so a divergent path means every cache key misses across the boundary. The default profile and the `withToolchain` variant both set this field to the toolchain they were built from.

Mounts (host source → literal container dest; literal dests avoid the `~`-expands-on-host-launcher gotcha):

- `~/.cargo/registry` → `/home/wrapix/.cargo/registry` (rw, optional) — shared crate cache between host and sandbox; pre-warms at launch and writes back as cargo downloads crates not in the pre-warm set. `ro` here breaks any cargo command that needs a fresh crate (`Read-only file system (os error 30)` writing to `registry/index/.../.cache/...` or `registry/cache/...`), since cargo's pre-fetch path is the same as its on-demand download path.
- `~/.cargo/git` → `/home/wrapix/.cargo/git` (rw, optional) — shared git dependency cache; same rw rationale as registry (cargo writes new git checkouts here on cache miss).
- `~/.cache/sccache` → `/home/wrapix/.cache/sccache` (rw, optional) — shared sccache store between host and sandbox

Writable dirs (`writableDirs = [ "/home/wrapix/.cargo" ]`): on Linux, the launcher stacks a tmpfs at `/home/wrapix/.cargo` with `U=true` so the dir is wrapix-owned. Without this, podman creates the mountpoint parent as root (to host the registry/git binds on top, regardless of mount mode) and cargo can't write `.global-cache`/`credentials.toml` there. Darwin doesn't need the fix — its entrypoint creates these dirs via `mkdir -p` as namespaced-root-mapped-to-`HOST_UID`, already wrapix-writable.

Network allowlist: `crates.io`, `static.crates.io`, `index.crates.io`

**Rust package builder (`profile.buildPackage`).** The rust profile owns Rust package construction the same way it owns the toolchain, sccache wiring, and cache mounts. `buildPackage` is a crane-backed function that produces a binary derivation alongside separate clippy and nextest derivations, so consumers in `devShells.default.packages` (e.g. `packages.loom`) rebuild only `bin` on workspace edits — `clippy` and `nextest` are realized separately by `nix flake check`. Crane's `cargoArtifacts` snapshot caches dep compilation across rebuilds — the build-sandbox analog of the sccache mounts that cover the interactive cargo path; a `Cargo.lock` edit invalidates `cargoArtifacts`, but a workspace-source edit does not.

Signature:

```nix
profile.buildPackage {
  src;                            # main source; filtered via crane.cleanCargoSource (or srcFilter when set)
  cargoLock;                      # path to Cargo.lock
  extraSrcs ? { };                # { "rel/dest" = ./abs-source; ... } — staged for clippy + nextest only
  cargoArtifacts ? null;          # optional pre-computed artifacts; default: derived from src + cargoLock
  cargoExtraArgs ? "";            # passed through to cargo (e.g. "--features foo", "--profile dev")
  buildInputs ? [ ];
  propagatedBuildInputs ? [ ];
  nativeBuildInputs ? [ ];
  meta ? { };
  srcFilter ? null;               # optional path-type predicate; replaces filterCargoSources when set
}
=> { bin; clippy; nextest; cargoArtifacts; }
```

- `bin` is the output a `packages.<name>` entry consumes; it has **no** dependency edge on `clippy` or `nextest`. Because of that missing edge, consumers depending only on `bin` (e.g. the devshell) realize only `bin` on workspace edits; `clippy` and `nextest` have separate, unrealized store paths until `nix flake check` asks for them.
- `clippy` and `nextest` are independent derivations the flake wires into `nix flake check` (one entry per check in `tests/default.nix`); they are the only outputs that see `extraSrcs`. `cargoArtifacts` and `bin` close over `src` only.
- `extraSrcs` exists for test inputs the harness reads from outside the workspace (e.g. loom's `crates/loom/tests/annotations.rs` reads `specs/*.md`). Editing files in `extraSrcs` invalidates `clippy`/`nextest` but leaves `bin` and `cargoArtifacts` untouched — devshell-style consumers stay warm across test-fixture edits (e.g. `specs/*.md` changes).
- `srcFilter` is the escape hatch for crates that need non-Rust files in `src` at compile time (e.g. loom-templates' askama `#[template(path = ...)]` reads `templates/*.md`). The default `null` keeps the spec invariant that editing `src/README.md` does not invalidate `bin`; passing a custom predicate lets the consumer broaden it (`craneLib.filterCargoSources` is exposed via `profile.craneLib` so callers can compose: `path: type: (craneFilter path type) || (lib.hasInfix "/templates/" path)`).
- `cargoArtifacts` is exposed as an output and accepted as an input so workspaces with multiple binaries can share dep compilation across calls. Single-binary callers ignore both directions.
- `buildPackage` closes over `profile.toolchain` via crane's `overrideToolchain`, so `bin`/`clippy`/`nextest` resolve `rustc` to the same `/nix/store/...` path as the sandbox image and the host devshell PATH. `withToolchain` rebuilds `buildPackage` against the custom toolchain alongside `packages`/`env`/`shellHook`/`toolchain`.
- Builds are pure — Nix-sandboxed, no `__noChroot`. Sccache covers the host/sandbox/sibling-app cargo paths (where it's mounted from `~/.cache/sccache` and persists across runs); `cargoArtifacts` is the equivalent caching layer for build-sandbox cargo invocations. The two layers do not overlap and do not share cache state.
- Always builds for `pkgs.stdenv.hostPlatform.system` — no cross-compilation. Returns one `bin` per call — workspaces with multiple binaries call `buildPackage` once per binary, threading the same `cargoArtifacts` through.

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
> (see *Downstream Integration*).
>
> **Why stable rust-analyzer?** rust-analyzer ships from
> `fenix.stable.rust-analyzer-preview`, a manifest download channel-aligned
> with the stable toolchain — *not* `fenix.packages.${system}.rust-analyzer`,
> which is built from source against the nightly branch. The from-source
> nightly RA drags a full nightly cargo/rustc/rust-std closure into every
> downstream flake on each input update (its source input tracks the nightly
> branch tip, so fenix's lock advances daily). Stable RA lags nightly by
> ~6 weeks; consumers who need nightly RA can opt in per-flake via
> `deriveProfile profiles.rust { packages = [ fenix.packages.${system}.rust-analyzer ]; }`
> and accept the closure cost themselves.

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
| `flake.nix` | Declares `fenix`, `crane`, and `treefmt-nix` flake inputs; threads `fenix`, `crane`, and the project treefmt wrapper to `lib/sandbox/profiles.nix` via `lib/sandbox/default.nix` |
| `lib/default.nix` | Defines `deriveProfile` (merges `packages` / `mounts` / `env` / `networkAllowlist`; passes through other profile fields) |
| `lib/sandbox/default.nix` | Imports `profiles.nix` with Linux `pkgs`, host `pkgs`, `fenix`, `crane`, and the treefmt wrapper; defines internal `extendProfile` helper |
| `lib/sandbox/profiles.nix` | Defines `mkProfile`, `basePackages`, built-in `base`/`rust`/`python` profiles; builds rust toolchain via `fenixPkgs.stable.defaultToolchain` combined with `rust-src` and `rust-analyzer`; wires sccache and sccache env vars, `CARGO_INCREMENTAL=0`; exposes the per-profile host `shellHook` (rust prepends `${toolchain}/bin` to host PATH and re-exports sccache env); exposes `profile.toolchain` on the rust profile (default and `withToolchain` variants) so consumer-side derivations can share the sandbox's toolchain store path; defines `profile.buildPackage` on the rust profile via `(crane.mkLib pkgs).overrideToolchain (_: profile.toolchain)`, returning `{ bin; clippy; nextest; cargoArtifacts }` |
| `lib/loom/default.nix` | Thin consumer of `wrapix.profiles.rust.buildPackage`; passes `extraSrcs` for `tests/loom/{mock-pi,mock-claude}` and `specs/`; returns the full `{ bin; clippy; nextest; cargoArtifacts }` attrset so the flake can wire each output independently |
| `lib/mcp/tmux/mcp-server.nix` | Thin consumer of `wrapix.profiles.rust.buildPackage`; passes `buildInputs`/`propagatedBuildInputs = [ pkgs.tmux ]`; returns the full attrset |
| `modules/flake/packages.nix` | `packages.loom` and `packages.tmux-mcp` consume `.bin` from their respective `buildPackage` invocations (no test/lint dep edge into the devshell rebuild path) |
| `tests/default.nix` | `checks` includes `loom-clippy`, `loom-nextest`, `tmux-mcp-clippy`, `tmux-mcp-nextest` from the same `buildPackage` outputs |
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

# Build a Rust package whose devshell rebuild path skips lint/test.
# `bin` becomes packages.<name>; `clippy` and `nextest` become check entries.
let
  loom = profiles.rust.buildPackage {
    src = ./loom;
    cargoLock = ./loom/Cargo.lock;
    extraSrcs = {
      "tests/loom/mock-pi"     = ./tests/loom/mock-pi;
      "tests/loom/mock-claude" = ./tests/loom/mock-claude;
      "specs"                  = ./specs;
    };
    nativeBuildInputs = [ pkgs.git ];
  };
in {
  packages.loom        = loom.bin;
  checks.loom-clippy   = loom.clippy;
  checks.loom-nextest  = loom.nextest;
}
```

### Profile-Specific Configuration

Profiles may expose functions for configuration specific to their toolchain.
These live on the profile attrset itself, not on `deriveProfile`.

- `profiles.rust.withToolchain` — accepts `{ file, sha256 }` (the `file` is the
  path to `rust-toolchain.toml`; `sha256` is the hash of the downloaded components),
  returns a new profile attrset (without `withToolchain`) using
  `fenix.fromToolchainFile`. `rust-src` and `stable.rust-analyzer-preview` are
  combined in on top of whatever components the toolchain file declares.
  `packages`, `env`, `shellHook`, `toolchain`, and `buildPackage` are all
  rebuilt against the custom toolchain so they close over the same derivation —
  `env.RUST_SRC_PATH` resolves to the new toolchain's stdlib source, the host
  PATH prepend in `shellHook` resolves to the same `rustc` binary the sandbox
  uses, `profile.toolchain` is exactly that derivation for downstream
  `runtimeInputs`, and `profile.buildPackage`'s `bin`/`clippy`/`nextest` outputs
  compile against it.

  **Caveat:** do not list `rust-analyzer` in your `rust-toolchain.toml`
  `components`. The profile always combines `stable.rust-analyzer-preview` on
  top, and `fenix.combine` errors on duplicate `bin/rust-analyzer`. Consumers
  who want a different RA should omit it from `components` and add their
  preferred build via `deriveProfile profiles.rust { packages = [ ... ]; }`
  instead.

- `profiles.rust.buildPackage` — see [Rust package builder](#rust-profile)
  above. Closes over `profile.toolchain`; rebuilt under `withToolchain`.

Only the Rust profile has `withToolchain` and `buildPackage`. Other profiles
(base, python) do not expose profile-specific configuration functions.

## Profile-Image Manifest

Loom and other multi-profile orchestrators need to dispatch to per-profile
images at runtime — one bead might want `profile:rust`, the next
`profile:python`. The launcher (`packages.wrapix`) is profile-agnostic, so
the profile→image mapping lives outside it as a JSON manifest.

`wrapix.lib.${system}.mkProfileImages` produces that manifest:

```nix
wrapix.lib.${system}.mkProfileImages {
  base   = (wrapix.mkSandbox { profile = profiles.base;   }).image;
  rust   = (wrapix.mkSandbox { profile = profiles.rust;   }).image;
  python = (wrapix.mkSandbox { profile = profiles.python; }).image;
  myCustom = (wrapix.mkSandbox { profile = myCustomProfile; }).image;
}
```

The output is a `pkgs.writeText "profile-images.json" <…>` derivation whose
content is a JSON object keyed by profile name. Each value is `{ ref,
source }` — `ref` is the podman image reference (`localhost/wrapix-<name>:<hash>`),
`source` is the Nix store path the launcher hands to `podman load`. Both
fields are computed Nix-side from the image derivation so consumers never
re-implement the tag logic. Loom maps the manifest entry's `ref` and
`source` to `SpawnConfig.image_ref` and `SpawnConfig.image_source`
respectively when building each spawn-config.

The manifest is keyed by profile only. Agent runtime (claude vs pi) is
selected at container start via `WRAPIX_AGENT` (see
[loom-agent.md — Agent Runtime Layer](loom-agent.md#agent-runtime-layer)) —
each profile image installs both runtimes, so a single
`packages.image-<profile>` covers both agents.

The repo's bundled manifest covering `base`, `rust`, `python` is exposed as
`packages.profile-images`. External flakes adding custom profiles call
`mkProfileImages` themselves to produce a manifest covering their full
profile set, then point `LOOM_PROFILES_MANIFEST` at it.

## Flake Outputs

Profiles surface as three sibling output families:

| Output | Shape | Use |
|--------|-------|-----|
| `packages.image-<profile>` | OCI artifact (Linux: `streamLayeredImage`; Darwin: tarball); both agent runtimes installed | Consumers driving podman directly; manifest entries |
| `packages.sandbox-<profile>[-pi]` | `makeWrapper` of `packages.wrapix` + `packages.image-<profile>`; bare form defaults to `WRAPIX_AGENT=claude`, `-pi` suffix sets `WRAPIX_AGENT=pi` | One-shot users (`nix run .#sandbox-rust`, `nix run .#sandbox-rust-pi`) |
| `packages.profile-images` | JSON manifest from `mkProfileImages`, keyed by profile (not by profile×agent) | Loom (`LOOM_PROFILES_MANIFEST`) |

`<profile>` covers the built-in profiles (`base`, `rust`, `python`). The
`-mcp` axis (runtime MCP server selection) is independent of agent and
remains its own family of outputs in `modules/flake/packages.nix`.

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
- [ ] uv cache mount is writable so uv can fetch packages not in the pre-warm set without `Read-only file system` errors
  [judge](../tests/judges/profiles.sh#test_uv_cache_writable)
- [ ] deriveProfile correctly merges packages and environment
  [judge](../tests/judges/profiles.sh#test_derive_profile_merge)
- [ ] Profiles are composable (can extend extended profiles)
  [system](bash tests/mcp/tmux/e2e/test_profile_composition.sh)
- [ ] Host devshell that splices `rustProfile.shellHook` resolves `rustc` to the same `/nix/store/...` path as the sandbox
  [judge](../tests/judges/profiles.sh#test_host_sandbox_rustc_same_store_path)
- [ ] `profile.toolchain` is exposed on both the default rust profile and `withToolchain { file; sha256; }`, and points at the same derivation referenced by the profile's `packages` and the `${toolchain}/bin` PATH prepend in `shellHook`
  [judge](../tests/judges/profiles.sh#test_rust_toolchain_field)
- [ ] `profiles.rust` and `profiles.rust.withToolchain { ... }` closures contain zero `*-nightly-*` derivations after a fresh `nix flake update` (regression guard against reintroducing `fenix.packages.${system}.rust-analyzer`, which drags a nightly cargo/rustc/rust-std closure). Implemented as a deterministic shell test (nix-eval the toolchain `drvPath`, scan the closure for `*-nightly-*` paths, exit 0/1) — `[check]` rather than `[judge]` because there is no rubric ambiguity for an LLM to judge.
  [system](bash tests/profiles/no-nightly-closure.sh test_no_nightly_closure)
- [ ] `mkProfileImages { rust = …; }` produces a JSON file whose entry for `rust` has both `ref` and `source` fields, with `source` resolving to the same store path as `(mkSandbox { profile = profiles.rust; }).image`
  [system](bash tests/profiles/profile-images-manifest.sh test_manifest_shape)
- [ ] `packages.image-<name>`, `packages.sandbox-<name>`, and `packages.profile-images` all evaluate for each built-in profile
  [system](bash tests/profiles/profile-images-manifest.sh test_flake_outputs_present)
- [ ] `profiles.rust.buildPackage` is exposed and returns an attrset with `bin`, `clippy`, `nextest`, and `cargoArtifacts` fields
  [system](bash tests/profiles/build-package.sh test_build_package_exposed)
- [ ] Editing a workspace source file changes the `bin` derivation hash but does **not** change the `cargoArtifacts` derivation hash (dep cache reused across edits)
  [system](bash tests/profiles/build-package.sh test_workspace_edit_reuses_dep_cache)
- [ ] Source filter excludes non-Cargo files: editing a `README.md` or other `*.md` file inside `src` does **not** change the `bin`, `clippy`, or `nextest` derivation hashes
  [system](bash tests/profiles/build-package.sh test_source_filter_excludes_non_cargo)
- [ ] Editing a `.rs` file invalidates `bin`, `clippy`, and `nextest` together (the workspace source closure is shared by all three) but does **not** invalidate `cargoArtifacts`
  [system](bash tests/profiles/build-package.sh test_workspace_edit_skips_cargo_artifacts)
- [ ] Editing a file in `extraSrcs` invalidates `clippy` and `nextest` but does **not** invalidate `bin` or `cargoArtifacts`
  [system](bash tests/profiles/build-package.sh test_extra_srcs_scoped_to_lint_test)
- [ ] `bin`, `clippy`, and `nextest` all close over `profile.toolchain`, so `${toolchain}/bin/rustc` resolves to the same `/nix/store/...` path across all three derivations, on both `profiles.rust` and `profiles.rust.withToolchain { ... }`
  [system](bash tests/profiles/build-package.sh test_build_package_toolchain_alignment)
- [ ] `lib/loom/default.nix` and `lib/mcp/tmux/mcp-server.nix` are thin `wrapix.profiles.rust.buildPackage` consumers (no direct `pkgs.rustPlatform.buildRustPackage` or `makeRustPlatform` call); `packages.loom` and `packages.tmux-mcp` consume `.bin`; `tests/default.nix` exposes `loom-clippy`, `loom-nextest`, `tmux-mcp-clippy`, `tmux-mcp-nextest` checks
  [system](bash tests/profiles/build-package.sh test_consumers_migrated)

## Out of Scope

- Language-specific project scaffolding
- IDE configuration beyond Claude Code
- Auto-detection of `rust-toolchain.toml` at runtime (must be passed explicitly via `withToolchain`)
- Automatic pruning of cargo registry / git / uv caches — operators are expected to clean `~/.cargo/{registry,git}` and `~/.cache/uv` manually if they grow unbounded; sccache is self-capped via `SCCACHE_CACHE_SIZE`.
- Tracking nightly rust-analyzer in the default profile. Building `fenix.packages.${system}.rust-analyzer` from source pulls a matching nightly cargo/rustc/rust-std closure into every consumer's flake on each input update; the profile pins `fenix.stable.rust-analyzer-preview` instead. Consumers who need nightly RA opt in via `deriveProfile`.
- Cross-compilation in `buildPackage`. Always builds for `pkgs.stdenv.hostPlatform.system`. Consumers needing cross builds drop down to crane directly.
- Returning multiple `bin` outputs from a single `buildPackage` call. Workspaces with multiple binary crates call `buildPackage` once per binary, threading the same `cargoArtifacts` through to share dep compilation.

## Downstream Integration

Cross-boundary sccache hits require the host shell, sandbox image, and any
sibling Nix app derivations that run cargo to all reference the same toolchain
derivation. Wrapix exposes two surfaces for this — splice `rustProfile.shellHook`
into the host devShell, and use `rustProfile.toolchain` in `runtimeInputs` of
any sibling derivation that runs cargo. Both close over the same `/nix/store/...`
path the sandbox bakes in.

For the host devshell, splice the rust profile's `shellHook`:

```nix
let
  rustProfile = wrapix.profiles.rust.withToolchain {
    file = ./rust-toolchain.toml;
    sha256 = "sha256-...";
  };
  sandbox = wrapix.mkSandbox { profile = rustProfile; };
in pkgs.mkShell {
  shellHook = ''
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
