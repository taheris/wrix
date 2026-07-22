# Profiles System

Pre-configured development environments with language-specific toolchains.

## Problem Statement

Different projects need different toolchains, but every project benefits from a shared agent-tooling floor (shell, search, VCS, formatters, and stdlib-only scripting). Language profiles add project-specific toolchains and supporting tools on top of that floor and expose extension hooks (`deriveProfile`, per-toolchain constructors like `rustProfile`) so consumers can pin versions or add packages without re-implementing the bundle. The same profile carries separate sandbox-image and host-devshell package surfaces, keeping toolchain identity consistent across the boundary without mixing host and image derivations.

## Architecture

A profile is a Nix attrset that bundles image packages (`packages`), host
packages (`hostPackages`), non-secret `env`, `runtimeSecrets`, `mounts`,
`networkAllowlist`, `shellHook`, plus optional fields and toolchain-specific extras
(`profile.toolchain` and
`profile.buildPackage` on the rust profile). Built-in profiles ship as
constants at `profiles.<name>`; per-toolchain pinned variants come from
top-level constructors (`rustProfile { toolchain; sha256; }`). Profile
extension uses the universal `deriveProfile` operator — there is no
on-profile `withX` builder.

Two peer consumers take a profile and produce a consumer-facing artifact:
`mkSandbox` consumes image-platform `profile.packages` to produce an OCI
image source and launcher wrapper; `mkDevShell` consumes host-platform
`profile.hostPackages` to produce a host devshell. Both close the loop on
toolchain identity by referencing the same `profile.toolchain` derivation.
That single `/nix/store/...` path lands in the sandbox image at build time,
in the host devshell's PATH via `mkDevShell`, and in sibling-app
`runtimeInputs` via direct field access. The shared store path is what makes
cross-boundary sccache hits possible — the invariants and success criteria
below exist to keep that identity from drifting.

`mkDevShell` additionally manages workspace lifecycle integration: it starts
`services.md`'s per-workspace service container when beads or the project Nix
cache is enabled, and it points `core.hooksPath` at the hook derivation selected
by `prekHooks`; see [Prek hook management](#prek-hook-management) for the
devshell selection rules. Durable host/Loom Git setup outside devshell entry is
owned by `cli.md`'s `wrix init`.

Rust toolchains are baked into the container image at build time, never
bootstrapped from rustup at container start. fenix supplies proper Nix
derivations with stable dynamic linkers, eliminating the rustup-on-Nix
breakage mode where nixpkgs updates evict the glibc path a downloaded
rustup binary was linked against.

## Profile Attrset Schema

A profile is a Nix attrset produced by the internal `mkProfile` helper. Fields:

| Field | Type | Purpose |
|-------|------|---------|
| `name` | string | Profile identifier (e.g. `"base"`, `"rust"`) |
| `packages` | list of derivations | Image-platform packages baked into the container image. This is the image package surface, not the host devshell PATH source. |
| `hostPackages` | list of derivations | Host-platform packages placed on the `mkDevShell` PATH. This is the host package surface; it does not get baked into the image. |
| `corePackages` | list of derivations | The wrix-controlled, fixed-per-instance subset of `packages` — the `basePackages` floor plus the profile toolchain (default or pinned). Set at construction by `mkProfile`/`rustProfile`; downstream extension never appends to it. The image builder assigns it to the stable layer tier and treats `packages` − `corePackages` as the downstream-added delta that rides in the volatile leaf tier (see `image-builder.md` § Provenance-Tiered Layering). |
| `env` | attrset of strings | Non-secret environment defaults set inside the container and baked into image metadata |
| `runtimeSecrets` | attrset of `"optional"` / `"required"` | Validated environment-variable names whose values the host launcher resolves at runtime; values are absent from the profile, Nix store, and image |
| `mounts` | list of mount specs | Host → container bind mounts; each `{ source, dest, mode, optional }` |
| `networkAllowlist` | list of strings | Domains permitted when `WRIX_NETWORK=limit` (merged with base allowlist) |
| `enabledPlugins` | attrset | Claude Code plugins merged into `~/.claude/settings.json` (e.g. `"rust-analyzer-lsp@claude-plugins-official" = true`) |
| `shellHook` | shell snippet | Internal alignment hook spliced by `mkDevShell`. Aligns host-side toolchain identity, env, and PATH with the sandbox so `rustc` resolves to the same `/nix/store/...` path on both sides — the prerequisite for cross-boundary sccache hits and shared `target/` artifact reuse. Consumers do not splice this directly; they pass the profile to `mkDevShell { profile = ...; }`. |
| `writableDirs` | list of strings | Linux-only: paths where the launcher stacks a tmpfs with `U=true` so the dir is wrix-owned — needed because podman creates bind-mount parents as root, which blocks writes to sibling files like `.global-cache`/`credentials.toml` |

Mount specs use `optional = true` to mean "skip this bind silently if the host source path does not exist", letting profiles declare cache mounts that no-op on hosts that haven't yet populated them.

`deriveProfile` merges `packages`, `hostPackages`, `mounts`, `env`, `runtimeSecrets`, and `networkAllowlist` (package/mount/allowlist lists concatenated; env and runtime-secret attrsets right-biased). Extension `packages` append to the image package surface only; extension `hostPackages` append to the host devshell surface only. Neither package surface crosses into the other. `corePackages` passes through from the base unchanged, so the wrix-controlled floor + toolchain stays distinguishable from downstream additions for image layer tiering (`image-builder.md` § Provenance-Tiered Layering). Other fields (`name`, `enabledPlugins`, `shellHook`, `writableDirs`) pass through from the extensions attrset if set, otherwise inherit from the base — they are not deep-merged. Callers extending a profile with extra plugins or shell hooks must compose those values themselves.

The rust profile additionally exposes `toolchain` (the resolved fenix `combine` derivation) and `buildPackage` (a crane-backed Rust package builder); both pass through `deriveProfile` since extensions don't override them. For project-pinned rust toolchains, consumers use the top-level `rustProfile { toolchain; sha256; }` constructor — see [wrix.rustProfile](#wrixrustprofile) and [Rust Profile](#rust-profile) for details.

## Built-in Profiles

### Base Profile

Curated developer toolkit. The rust and python profiles extend this set. Grouped by purpose:

| Category | Packages |
|----------|----------|
| Shell + POSIX core | bash, coreutils, diffutils, findutils, gawk, gnugrep, gnused, gnutar, gnumake, gzip, less, patch, rsync, tree, unzip, util-linux, whichQuiet, zip |
| File + text | fd, file, ripgrep, vim |
| Network + process | curl, iproute2, nftables, iptables, libcap (capsh), iputils, lsof, netcat, openssh, procps |
| Data + ad hoc scripting | jq, python3, yq |
| Package manager | nix |
| VCS + PRs | git, gh |
| Issue tracker | beads (`bd`), dolt, gc |
| Agent tooling | man, prek, shellcheck, tmux, treefmt (wrapped with project formatters) |

`mkSandbox` adds the Linux-built `wrix` CLI to every resolved sandbox profile,
so `wrix beads push` is available in containers without making the reusable
profile attrset depend on the wrix workspace build.

`whichQuiet` is a local `pkgs.which` wrapper that suppresses `"no X in (PATH)"` noise.

`treefmt` is the project-wide formatter wrapper (nixfmt, rustfmt, shellcheck, deadnix, statix) built via `treefmt-nix.lib.mkWrapper`. Including it in the base profile ensures every consumer gets the same formatters.

**Base env:** none. `ANTHROPIC_API_KEY`, `CLAUDE_CODE_OAUTH_TOKEN`, and `OPENAI_API_KEY` are optional runtime-secret declarations, not static env.
**Base mounts:** none. Host `~/.claude` is intentionally NOT mounted — containers use `$PROJECT_DIR/.claude` so user-level settings stay separate from project-level settings.
**Base network allowlist:** `api.anthropic.com`, `github.com`, `ssh.github.com`, `cache.nixos.org` — always permitted regardless of profile, used only when `WRIX_NETWORK=limit`.

### Rust Profile

Extends base with Rust toolchain via `nix-community/fenix`. See *Why not rustup?* and *Why fenix?* below for the design rationale.

**Toolchain:** `fenix.packages.${system}.stable.defaultToolchain` combined with
`rust-src` (separately pinned) and `stable.rust-analyzer-preview` (manifest build
from the stable channel — see *Why stable rust-analyzer?* below). The default
toolchain ships the same components rustup installs by default: rustc + cargo +
rust-std + clippy + rustfmt + rust-docs.

| Package | Purpose |
|---------|---------|
| fenix toolchain | cargo, rustc, clippy, rustfmt, rust-std |
| fenix rust-src | Standard library source for rust-analyzer |
| fenix `stable.rust-analyzer-preview` | LSP server (manifest build, channel-aligned with stable) |
| cargo-nextest | Rust test runner |
| sccache | Shared compile cache across host + sandbox |
| gcc | C compiler for linking |
| openssl | TLS library (runtime) |
| openssl.dev | TLS headers (separate Nix output) |
| pkg-config | Library discovery |
| postgresql.lib | Database client libs |

Environment:

- `CARGO_HOME` — intentionally **unset**; cargo's `$HOME/.cargo` default applies, which resolves to `/home/wrix/.cargo` inside the container and to the user's host home in `mkDevShell`. The registry/git mount dests below match cargo's container default, so the shared host cache lines up without an explicit env override. Non-mounted CARGO_HOME state (credentials.toml, config.toml, `cargo install` bins) lives on tmpfs on Linux and is ephemeral across container runs — intentional for an agent-style environment.
- `RUST_SRC_PATH=${toolchain}/lib/rustlib/src/rust/library` — rust-analyzer standard library resolution
- `LIBRARY_PATH=${pkgs.postgresql.lib}/lib` — PostgreSQL library discovery at link time
- `OPENSSL_INCLUDE_DIR=${pkgs.openssl.dev}/include` — OpenSSL headers
- `OPENSSL_LIB_DIR=${pkgs.openssl.out}/lib` — OpenSSL libraries
- `RUSTC=${toolchain}/bin/rustc` — pass the selected compiler to Cargo as an absolute path so a long-lived sccache server can execute the correct toolchain across repositories
- `RUSTC_WRAPPER=${pkgs.sccache}/bin/sccache` — route compiler invocations through sccache
- `CARGO_BUILD_RUSTC_WRAPPER=${pkgs.sccache}/bin/sccache` — same value, picked up by cargo directly
- `SCCACHE_DIR=/home/wrix/.cache/sccache` — stable in-container cache path; its optional host mount shares data, and `/home/wrix/.cache` remains writable via tmpfs when that mount is absent
- `SCCACHE_CACHE_SIZE=50G` — ceiling above sccache's 10 GiB default; the default LRU-evicts mid-build for workspace-sized Rust projects. Changing this requires `sccache --stop-server` before the server picks up the new value.
- `CARGO_INCREMENTAL=0` — sccache refuses to cache any `rustc` invocation with `-C incremental=...`; disabling incremental lets every Rust compile flow through sccache instead.
- `CARGO_TARGET_DIR` — intentionally **unset**; cargo's per-workspace default (`<workspace>/target`) applies. Pinning `CARGO_TARGET_DIR` to a shared path across workspaces defeats cargo's freshness tracking and churns builds.

**Host devshell alignment.** The profile's `shellHook` prepends `${toolchain}/bin` to `PATH` and re-exports `RUSTC` as the toolchain's absolute compiler path alongside `RUSTC_WRAPPER`, `SCCACHE_DIR`, `SCCACHE_CACHE_SIZE`, and `CARGO_INCREMENTAL=0`. The absolute compiler path lets host dev shells with different toolchain pins share one long-lived sccache server without relying on the server's startup `PATH`. Without the PATH prepend, host PATH falls through to rustup's `rustc` (or whichever appears first), and the diverging sysroot path baked into rlib metadata invalidates every sccache key across the boundary — even when both sides report the same Rust version. Consumers reach this alignment by passing the rust profile to `mkDevShell { profile = ...; }`, which splices `profile.shellHook` automatically — there is no consumer-facing splice path. `rustProfile { toolchain; sha256; }` rebuilds the snippet over the project-pinned toolchain so `packages`, `hostPackages`, `env`, `shellHook`, and `toolchain` (see below) all close over the same derivation.

**Toolchain derivation (`profile.toolchain`).** The rust profile exposes the resolved fenix `combine` derivation as `profile.toolchain` — the same store path interpolated into `shellHook`'s PATH prepend and shared by `buildPackage`'s craneLib. Sibling Nix apps that run cargo (e.g. `pkgs.writeShellApplication { runtimeInputs = [ rustProfile.toolchain ]; ... }`) must point `runtimeInputs` at this field rather than re-instantiating fenix in their own flake. Re-instantiation produces a different `/nix/store/...` path even when fenix versions match, and again when calling `fromToolchainFile` directly (bare `rust-<ver>` vs the `combine`-wrapped `rust-mixed`); sccache hashes the compiler binary, so a divergent path means every cache key misses across the boundary. Both `profiles.rust` (the unpinned default) and `rustProfile { toolchain; sha256; }` (the project-pinned constructor) set this field to the toolchain they were built from.

**Host/image toolchain split.** `profile.toolchain` resolves to the *host-platform* fenix derivation so the devshell PATH prepend and `buildPackage`'s craneLib produce binaries that run on the user's machine. The image's toolchain (the one in `profile.packages` and `RUST_SRC_PATH`) targets the image platform (Linux). On Linux hosts the two derivations coincide; on Darwin hosts they share the channel/version but resolve to different `/nix/store/...` paths. Cross-boundary sccache reuse (host ↔ sandbox) therefore works only on Linux — consistent with the Darwin cache-mount caveat below.

Mounts (host source → literal container dest; literal dests avoid the `~`-expands-on-host-launcher gotcha):

- `~/.cargo/registry` → `/home/wrix/.cargo/registry` (rw, optional) — shared crate cache between host and sandbox; pre-warms at launch and writes back as cargo downloads crates not in the pre-warm set. `ro` here breaks any cargo command that needs a fresh crate (`Read-only file system (os error 30)` writing to `registry/index/.../.cache/...` or `registry/cache/...`), since cargo's pre-fetch path is the same as its on-demand download path.
- `~/.cargo/git` → `/home/wrix/.cargo/git` (rw, optional) — shared git dependency cache; same rw rationale as registry (cargo writes new git checkouts here on cache miss).
- `~/.cache/sccache` → `/home/wrix/.cache/sccache` (rw, optional) — shared sccache store between host and sandbox

Writable dirs (`writableDirs = [ "/home/wrix/.cargo" "/home/wrix/.cache" ]`): on Linux, the launcher stacks tmpfs mounts at `/home/wrix/.cargo` and `/home/wrix/.cache` with `U=true` so those parents are wrix-owned. Without `/home/wrix/.cargo`, podman creates the cargo mountpoint parent as root (to host the registry/git binds on top, regardless of mount mode) and cargo can't write `.global-cache`/`credentials.toml` there. Without `/home/wrix/.cache`, podman can leave the sccache parent root-owned, and sccache cannot create or write `/home/wrix/.cache/sccache` when the optional host mount is absent. Darwin doesn't need the fix — its entrypoint creates these dirs via `mkdir -p` as namespaced-root-mapped-to-`HOST_UID`, already wrix-writable.

Network allowlist: `crates.io`, `static.crates.io`, `index.crates.io`

**Rust package builder (`profile.buildPackage`).** The rust profile owns Rust package construction the same way it owns the toolchain, sccache wiring, and cache mounts. `buildPackage` is a crane-backed function that produces a binary derivation alongside separate clippy and nextest derivations, so consumers in `devShells.default.packages` (e.g. a downstream flake's own Rust binary) rebuild only `bin` on workspace edits — `clippy` and `nextest` are realized separately by `nix flake check`. Crane's `cargoArtifacts` snapshot caches dep compilation across rebuilds — the build-sandbox analog of the sccache mounts that cover the interactive cargo path; a `Cargo.lock` edit invalidates `cargoArtifacts`, but a workspace-source edit does not.

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
- `extraSrcs` exists for test inputs the harness reads from outside the workspace (e.g. an integration test that reads fixture files under `tests/fixtures/`). Editing files in `extraSrcs` invalidates `clippy`/`nextest` but leaves `bin` and `cargoArtifacts` untouched — devshell-style consumers stay warm across test-fixture edits.
- `srcFilter` is the escape hatch for crates that need non-Rust files in `src` at compile time (e.g. an askama-style `#[template(path = ...)]` that reads `templates/*.md`). The default `null` keeps the spec invariant that editing `src/README.md` does not invalidate `bin`; passing a custom predicate lets the consumer broaden it. `craneLib.filterCargoSources` is exposed via `profile.craneLib` so callers can compose:

  ```nix
  path: type: (craneFilter path type) || (lib.hasInfix "/templates/" path)
  ```

- `cargoArtifacts` is exposed as an output and accepted as an input so workspaces with multiple binaries can share dep compilation across calls. Single-binary callers ignore both directions.
- `buildPackage` closes over `profile.toolchain` via crane's `overrideToolchain`, so `bin`/`clippy`/`nextest` resolve `rustc` to the same `/nix/store/...` path as the host devshell PATH. On Linux hosts this also matches the sandbox image's baked-in toolchain; on Darwin the image keeps a Linux toolchain while `buildPackage` produces host-platform binaries (see *Host/image toolchain split* above). `rustProfile { toolchain; sha256; }` rebuilds `buildPackage` against the project-pinned toolchain alongside `packages`/`hostPackages`/`env`/`shellHook`/`toolchain`.
- Builds are pure — Nix-sandboxed, no `__noChroot`. Sccache covers the host/sandbox/sibling-app cargo paths (where it's mounted from `~/.cache/sccache` and persists across runs); `cargoArtifacts` is the equivalent caching layer for build-sandbox cargo invocations. The two layers do not overlap and do not share cache state.
- Always builds for `pkgs.stdenv.hostPlatform.system` — no cross-compilation. Returns one `bin` per call — workspaces with multiple binaries call `buildPackage` once per binary, threading the same `cargoArtifacts` through.

> **Darwin caveat — rw cache mounts are session-scoped, not cross-boundary.**
> Apple's `container` CLI only exposes host paths via VirtioFS staging; the darwin
> entrypoint then `cp -r`s staged content into the profile's destination. That
> means writes to any rw cache mount inside a Darwin sandbox — `/home/wrix/.cargo/registry`,
> `/home/wrix/.cargo/git`, `/home/wrix/.cache/sccache`, and (per the Python profile)
> `/home/wrix/.cache/uv` — stay in the container's writable layer and are discarded
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
> `rust-toolchain.toml` files. Identical rustc store paths across the
> host/sandbox/sibling-app boundary — required for sccache hits — come
> from building the profile via `rustProfile { toolchain; sha256; }`,
> passing it to `mkDevShell { profile = ...; }` for the host devshell, and
> pointing sibling-app `runtimeInputs` at `profile.toolchain` (see
> *Downstream Integration*).
>
> **Why `rustProfile` over `fromToolchainFile` directly?** Both can read
> `rust-toolchain.toml`, but they produce different `/nix/store/...` paths
> even with identical inputs — `rustProfile` wraps the result in
> `fenix.combine` (yielding `rust-mixed`), while `fromToolchainFile`
> returns the bare `rust-<ver>` derivation. sccache hashes the compiler
> binary, so the divergent paths produce no cross-boundary hits. Consumers
> building a profile use `rustProfile`; consumers reaching for
> `runtimeInputs` use `profile.toolchain` from that built profile.
> `fromToolchainFile` is a fenix-native API the constructor builds on top
> of; downstream code does not call it directly.
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

Extends base with Python project tooling. The `python3` interpreter itself is inherited from the base profile so every sandbox has a stdlib-only scripting tool for small agent-authored utilities.

| Package | Purpose |
|---------|---------|
| uv | Fast package installer |
| ruff | Linter and formatter |
| ty | Type checker |

Environment:

- `UV_CACHE_DIR=/home/wrix/.cache/uv` — points at the cache mount dest below so uv reads from and writes back to the shared host cache (mirrors the rust profile's `SCCACHE_DIR` ↔ cache-mount alignment).

Mounts (host source → literal container dest):

- `~/.cache/uv` → `/home/wrix/.cache/uv` (rw, optional) — shared uv cache between host and sandbox; pre-warms at launch and writes back on cache miss. `ro` here would break any `uv` invocation that needs a package not in the pre-warm set, same failure mode as the cargo registry.

Network allowlist: `pypi.org`, `files.pythonhosted.org`

## API

The examples use `linuxPkgs` for image-platform derivations and `pkgs` for
host-platform derivations.

```nix
# Built-in unpinned profile (tracks fenix stable + rust-analyzer + rust-src)
wrix.mkSandbox { profile = wrix.profiles.rust; }

# Project-pinned rust profile via top-level constructor (fenix requires sha256 for purity)
wrix.mkSandbox {
  profile = wrix.rustProfile {
    toolchain = ./rust-toolchain.toml;
    sha256    = "sha256-...";
  };
}

# Pinned rust profile with an image-side package and env (single call, no deriveProfile needed)
wrix.mkSandbox {
  profile = wrix.rustProfile {
    toolchain = ./rust-toolchain.toml;
    sha256    = "sha256-...";
    packages  = [ linuxPkgs.sqlx-cli ];
    env       = { DATABASE_URL = "postgres://localhost/db"; };
    runtimeSecrets = { DATABASE_PASSWORD = "required"; };
  };
}

# Extend an existing profile (yours, a third party's, or a `wrix.rustProfile` result)
wrix.mkSandbox {
  profile = wrix.deriveProfile wrix.profiles.rust {
    packages = [ linuxPkgs.sqlx-cli ];
  };
}

# Add a tool to both the sandbox image and the host devshell by naming both surfaces.
let
  rustProfile = wrix.deriveProfile wrix.profiles.rust {
    packages = [ linuxPkgs.sqlx-cli ];
    hostPackages = [ pkgs.sqlx-cli ];
  };
in {
  devShells.default = wrix.mkDevShell { profile = rustProfile; };
  packages.image    = (wrix.mkSandbox { profile = rustProfile; }).image;
}

# Same profile drives both a host devshell and a sandbox image
let
  rustProfile = wrix.rustProfile {
    toolchain = ./rust-toolchain.toml;
    sha256    = "sha256-...";
  };
in {
  devShells.default = wrix.mkDevShell { profile = rustProfile; };
  packages.image    = (wrix.mkSandbox { profile = rustProfile; }).image;
}

# Sibling Nix app that runs cargo: reuse the same toolchain derivation so its
# `rustc` shares a /nix/store/... path with the sandbox image and the host
# devshell PATH.
let
  rustProfile = wrix.rustProfile {
    toolchain = ./rust-toolchain.toml;
    sha256    = "sha256-...";
  };
in pkgs.writeShellApplication {
  name = "test-ci";
  runtimeInputs = [ rustProfile.toolchain ];
  text = "cargo nextest run";
}

# Build a Rust package whose devshell rebuild path skips lint/test.
# `bin` becomes packages.<name>; `clippy` and `nextest` become check entries.
let
  myCrate = wrix.profiles.rust.buildPackage {
    src = ./my-crate;
    cargoLock = ./my-crate/Cargo.lock;
    extraSrcs = {
      "tests/fixtures" = ./tests/fixtures;
      "specs"          = ./specs;
    };
    nativeBuildInputs = [ pkgs.git ];
  };
in {
  packages.my-crate         = myCrate.bin;
  checks.my-crate-clippy    = myCrate.clippy;
  checks.my-crate-nextest   = myCrate.nextest;
}
```

## wrix.rustProfile

Top-level constructor for project-pinned rust profiles. Reads a
`rust-toolchain.toml` and produces a profile attrset whose `packages`,
`hostPackages`, `env`, `shellHook`, `toolchain`, and `buildPackage` all
close over the pinned fenix toolchain — the image derivation `mkSandbox`
bakes into the image and the host derivation `mkDevShell` prepends to PATH.

```nix
wrix.rustProfile {
  toolchain;                      # REQUIRED — path to rust-toolchain.toml
  sha256;                         # REQUIRED — fenix purity hash
  packages         ? [ ];         # appended to image-side profile.packages
  hostPackages     ? [ ];         # appended to host-side profile.hostPackages
  env              ? { };         # right-merged into profile.env (non-secret)
  runtimeSecrets   ? { };         # right-merged name → required/optional policy
  mounts           ? [ ];         # appended to profile.mounts
  networkAllowlist ? [ ];         # appended to profile.networkAllowlist
}
```

Both `toolchain` and `sha256` are required. The constructor uses
`fenix.fromToolchainFile` under the hood, then combines `rust-src` and
`stable.rust-analyzer-preview` on top of whatever components the toolchain
file declares. Extension args (`packages`/`hostPackages`/`env`/`runtimeSecrets`/`mounts`/
`networkAllowlist`) follow the same merge rules as `deriveProfile`: list
surfaces concatenate independently, while env and runtime-secret attrsets are
right-biased.

**Caveat:** do not list `rust-analyzer` in your `rust-toolchain.toml`
`components`. The constructor always combines `stable.rust-analyzer-preview`
on top, and `fenix.combine` errors on duplicate `bin/rust-analyzer`.
Consumers who want a different RA should omit it from `components` and
compose their preferred build via
`deriveProfile (rustProfile { ... }) { packages = [ ... ]; }` instead.

## mkDevShell

Single profile-aware entry point for host devshells. The safest path is
`sandbox.devShell { ... }`, where a `mkSandbox` result supplies the profile
and the configured `wrix` package together. `mkDevShell` remains the lower-level
helper for profile-only shells. Both paths splice a profile's `shellHook` into
a local shell — there is no manual splice path, no hand-copy alternative for
the profile's env exports, and no separate "add `profile.toolchain` to your
packages" step.

```nix
sandbox = wrix.mkSandbox {
  profile = rustProfile;
};

sandbox.devShell {
  packages  = [ ... ];        # optional host-only tools
  shellHook = "...";          # optional, appended after profile.shellHook
  env       = { ... };        # optional, right-merged into profile.env
  prekHooks = true;           # optional, see Prek hook management below
  nixCache  = true;           # optional, false disables services.md project cache
}

wrix.mkDevShell {
  profile   = rustProfile;   # REQUIRED — any profile attrset
  packages  = [ ... ];        # optional host-only tools appended after profile.hostPackages
  shellHook = "...";          # optional, appended after profile.shellHook
  env       = { ... };        # optional, right-merged into profile.env
  prekHooks = true;           # optional, see Prek hook management below
  nixCache  = true;           # optional, false disables services.md project cache
}
```

**Exactly one of `profile` or `sandbox` is required.** No default. A devshell
that needs no toolchain extension still passes `profile = profiles.base;`
explicitly, or uses `sandbox.devShell { }` from a concrete sandbox. Calling
`mkDevShell {}` without `profile` or `sandbox` errors at evaluation; passing
both is rejected because it can pair one profile with another sandbox's
configured `wrix` wrapper.
`sandbox.devShell { ... }` never accepts `profile` or `sandbox` overrides; it is
already bound to the sandbox object it came from.

Composition rules (deterministic, no consumer override):

| Field     | Rule                                                                                  |
|-----------|---------------------------------------------------------------------------------------|
| packages  | `profile.hostPackages ++ packages` (host-native profile tools are on PATH; image-only packages are not) |
| env       | `profile.env // env` (consumer wins on conflict)                                       |
| shellHook | `<wrix-internal lifecycle setup> + profile.shellHook + <consumer shellHook>` (fixed order) |

The internal lifecycle setup performs wrix-specific bootstrap (workspace
service startup for beads and the project Nix cache, dolt remote
configuration, prek `core.hooksPath` configuration — see
[Prek hook management](#prek-hook-management) below). It runs before
`profile.shellHook` so its tooling installs resolve through the system PATH
rather than the profile-extended one; the consumer's `shellHook` runs last so
consumer-set env can override profile-set env after the profile's exports have
fired.

`nixCache` defaults to `true` and enables the standard project cache owned by
`services.md`; `nixCache = false` is the explicit opt-out. The boolean form
uses the defaults below; attrset form customizes them:

```nix
nixCache = {
  enable = true;
  requireTrustedNix = true;

  publish = {
    packages = true;
    checks = true;
    devShell = true;
    includeRoots = [ ];
    excludeRoots = [ ];
  };

  warm = {
    packages = true;
    checks = false;
    devShell = true;
    includeRoots = [ ];
    excludeRoots = [ ];
  };

  warnSize = "50G";
  pendingTtl = "7d";
  pruneInterval = "24h";
};
```

`publish` roots are allowed to enter the cache when built or explicitly
published. `warm` roots are proactively built by `wrix service cache warm`;
checks are not warmed unless explicitly requested. `includeRoots` /
`excludeRoots` are flake installables or explicit flake attr paths, not raw
`/nix/store` paths, and excludes win. When the cache is enabled, `mkDevShell`
configures host Nix to pull from the local project cache and to publish
project-scoped builds back to it; if host Nix ignores the required trust or
post-build settings, devshell entry fails loud with remediation instead of
silently running cold. Shell entry does not eagerly evaluate all roots; when
the publish manifest is missing or stale it prints a short reminder unless
`WRIX_NIX_CACHE_REMINDER=0` is set.

### Prek hook management

The lifecycle owns the devshell side of `core.hooksPath` configuration for
prek-using repositories. `specs/pre-commit.md` owns the hook bundle export,
stage set, shim behavior, wrappers, and pre-push retry stamp; `cli.md` owns
`wrix init` hook setup for ordinary host Git and Loom driver worktrees outside
devshell entry. Two conditions gate the devshell install: (1)
`.pre-commit-config.yaml` exists in the working
directory, AND (2) `prekHooks` resolves to a derivation. When both hold, the
lifecycle runs `git config --local core.hooksPath ${derivation}` on every
devshell entry, pointing git at the selected Nix-store hook bundle. Consumers
choose the default bundle, opt out, or substitute a whole derivation via
`prekHooks`; they do not set `core.hooksPath` from their own shellHook or run
`prek install` as part of the `mkDevShell` contract.

`prekHooks` resolves as follows:

| Value | Resolves to |
|-------|-------------|
| `true` (the default) | the default hook bundle owned by `specs/pre-commit.md` |
| `false` | nothing — lifecycle is a no-op for hooks |
| a derivation | the substituted derivation, used as-is |

**Wrix-always-wins on stale config.** When `prekHooks` resolves to
a derivation (the `true` default or an explicit derivation), the
lifecycle overwrites `core.hooksPath` on the next devshell entry if
its current value differs from the target derivation's store path,
and prints a one-line message naming the old value so the consumer
notices the migration (e.g. a previous in-tree path the consumer was
using before adopting `prekHooks`). The `prekHooks = false` branch
is passive: the lifecycle does nothing, including not clearing a
`core.hooksPath` that a previous session set. Consumers who flip
from `true` to `false` and want the stale value cleared run
`git config --local --unset core.hooksPath` themselves.

Consumers needing a different hook bundle substitute a hand-built derivation
via `prekHooks = ...`. There is no parameterized constructor in v1; the exported
bundle and constructor deferral are owned by `specs/pre-commit.md`.

A consumer that wants a different shell wrapper (custom `pkgs.mkShell`
attrs, alternate hook layering) is reaching outside the contract — they
construct their own shell directly and accept that nothing in the wrix
profile system guarantees toolchain identity for that shell. The
ergonomic floor `mkDevShell` enforces is what makes cross-boundary
sccache alignment hard to misuse; bypassing `mkDevShell` re-opens every
failure mode this API closes.

## Profile-Image Manifest

Loom and other multi-profile orchestrators need to dispatch to per-profile
images at runtime — one bead might want `profile:rust`, the next
`profile:python`. The launcher (`packages.wrix`) is profile-agnostic, so
the profile→image mapping lives outside it as a JSON manifest.

`wrix.lib.${system}.mkProfileImages` produces that manifest:

```nix
wrix.lib.${system}.mkProfileImages {
  base     = (wrix.mkSandbox { profile = wrix.profiles.base;   }).image;
  rust     = (wrix.mkSandbox { profile = wrix.profiles.rust;   }).image;
  python   = (wrix.mkSandbox { profile = wrix.profiles.python; }).image;
  myCustom = (wrix.mkSandbox { profile = myCustomProfile;        }).image;
}
```

Each input is the `.image` value returned by `mkSandbox`: an image-source
derivation/attrset carrying the selected agent, source path, source kind,
digest, ref, and immutable `ProfileConfig` path.

The output is a `pkgs.writeText "profile-images.json" <…>` derivation whose
content is a JSON object keyed by profile name. Each value is keyed by the
image's selected agent (`direct`, `claude`, or `pi`) and contains `{ ref,
source, source_kind, profile_config }` — `ref` is the platform image reference
(`localhost/wrix-<name>:<hash>` on Linux), `source` is the Nix store path
installed by the runtime image installer, `source_kind` is the explicit source kind from
`image-builder.md` (`nix-descriptor` on Linux, `docker-archive` on Darwin),
and `profile_config` is the immutable `ProfileConfig` path passed to
`wrix spawn`. These fields are computed Nix-side from the image derivation so
consumers never re-implement the tag or source-kind logic. Loom maps the
selected agent variant's fields to `wrix spawn` inputs when building each
spawn-config.

The bundled `packages.profile-images` manifest covers the `direct` image for
each profile. `packages.profile-images-pi` covers the Pi images used by the
repo devshell's `LOOM_PROFILES_MANIFEST`. Agent overlays are separate image
variants: Claude is exposed by `packages.image-<profile>-claude`, and Pi is
exposed by `packages.image-<profile>-pi`.

External flakes adding custom profiles call `mkProfileImages` themselves to
produce a manifest covering their full profile set, then point
`LOOM_PROFILES_MANIFEST` at it.

## Flake Outputs

Profiles surface as three sibling output families:

| Output | Shape | Use |
|--------|-------|-----|
| `packages.image-<profile>` | OCI image source (Linux: archive-less `nix-descriptor`; Darwin: tarball `docker-archive`); built with `agent = "direct"` (the default base image) | Orchestrators that install images through wrix's platform install path; manifest entries |
| `packages.image-<profile>-claude` | OCI image source built with `agent = "claude"` and the platform `source_kind` | Orchestrators that install Claude images through wrix's platform install path |
| `packages.image-<profile>-pi` | OCI image source built with `agent = "pi"` and the platform `source_kind` | Orchestrators that install Pi images through wrix's platform install path |
| `packages.sandbox-<profile>` | Configured sandbox package with explicit `bin/wrix` plus `meta.mainProgram = "wrix-run"` for default `nix run`; direct agent variant | One-shot users (`nix run .#sandbox-rust`) |
| `packages.sandbox-<profile>-claude` | Configured Claude variant with explicit `bin/wrix` plus `wrix-run` main program | One-shot users that want Claude |
| `packages.sandbox-<profile>-pi` | Configured Pi variant with explicit `bin/wrix` plus `wrix-run` main program | One-shot users that want Pi; `packages.default` points at rust `sandbox-rust-pi` |
| `packages.sandbox-<profile>-mcp` | Direct-agent wrapper built with `mcpRuntime = true`, baking every registered MCP server and deferring selection to `WRIX_MCP` at launch | One-shot users that want runtime MCP server selection without a per-sandbox `mcp` build |
| `packages.sandbox-<profile>-<agent>-mcp` | Agent overlay wrapper built with both `agent = "<agent>"` and `mcpRuntime = true` | One-shot users that need a selected agent plus runtime MCP server selection |
| `packages.profile-images` | JSON manifest from `mkProfileImages`, keyed by profile then selected agent variant | External orchestrators (e.g. Loom via `LOOM_PROFILES_MANIFEST`) |

`<profile>` covers the built-in profiles (`base`, `rust`, `python`). The
`-mcp` axis is all-server runtime MCP selection, independent of agent, and
does not create per-server profile variants such as `-tmux` or `-playwright`.

## Downstream Integration

Cross-boundary sccache hits require the host shell, sandbox image, and any
sibling Nix app derivations that run cargo to all reference the same
toolchain derivation. The profile attrset is the carrier — build one via
`rustProfile { toolchain; sha256; }` and pass the same value to every
consumer.

For the host devshell, use `mkDevShell`:

```nix
let
  rustProfile = wrix.rustProfile {
    toolchain = ./rust-toolchain.toml;
    sha256    = "sha256-...";
  };
in {
  devShells.default = wrix.mkDevShell { profile = rustProfile; };
  packages.image    = (wrix.mkSandbox { profile = rustProfile; }).image;
}
```

`mkDevShell` splices `profile.shellHook` automatically (see
[mkDevShell](#mkdevshell) for composition rules) — no manual splice, no
hand-copied env exports, no separate `profile.toolchain` add to
`packages`. The same profile drives `mkSandbox` so host and sandbox close
over an identical `/nix/store/.../bin/rustc` derivation, matching sysroot
paths in rlib metadata, colliding sccache keys, and keeping cargo's
fingerprints in `/workspace/target/` valid across switches.

**Sibling Nix apps that run cargo.** `mkDevShell` covers the devshell;
sibling app derivations are a separate surface. Any
`pkgs.writeShellApplication`, `nix run` target, or CI runner in the
consumer's flake ships its own `runtimeInputs` and bypasses the devshell.
If that derivation re-instantiates fenix
(`inputs.fenix.packages.fromToolchainFile { ... }`), it gets a different
`/nix/store/...` path than the sandbox/host even when fenix versions
match, and an additional difference between the bare `rust-<ver>` output
and the `combine`-wrapped `rust-mixed` the profile builds. sccache hashes
the compiler binary, so cache keys never collide and an 11 GiB host cache
populated from inside the sandbox can produce 0 hits when invoked from
the sibling app.

The fix is to point `runtimeInputs` at `rustProfile.toolchain`, which is
the exact derivation the profile baked into the image and `mkDevShell`
prepended to PATH:

```nix
packages.test-ci = pkgs.writeShellApplication {
  name = "test-ci";
  runtimeInputs = [ rustProfile.toolchain ];
  text = "cargo nextest run";
};
```

Consumers with their own `rust-toolchain.toml` feed it to `rustProfile`
once; `rustProfile.toolchain` is then the `combine`-wrapped output of
THEIR file, identical across sandbox image, host devshell PATH (via
`mkDevShell`), and sibling-app `runtimeInputs`. They never need to call
fenix directly.

**Escape hatch.** If a downstream wants to control `rustc` independently
of wrix's pin (e.g., to use a different fenix revision on the host), it
can skip `rustProfile` and instead lock its fenix flake input to
wrix's:

```nix
inputs.wrix.url = "...";
inputs.fenix.url = "git+https://github.com/nix-community/fenix.git?ref=main";
inputs.wrix.inputs.fenix.follows = "fenix";
```

Without aligning toolchain identity through one of these surfaces
(`mkDevShell` + `mkSandbox` consuming the same profile, `profile.toolchain`
for sibling apps, or the `follows` escape hatch), host, sandbox, and
sibling derivations lock fenix to their own revisions, produce different
`/nix/store/.../bin/rustc` paths, and sccache entries do not cross the
boundary.

No downstream gitignore is required for the sandbox cache paths: mount
dests live under `/home/wrix/` inside the container, not under
`/workspace/`, so nothing is materialized in the project tree.

## Success Criteria

- Base profile provides functional development environment
  [judge](../tests/judges/profiles.sh#test_base_profile_functional)
- Base profile exposes `python3` on both image and host package surfaces for stdlib-only ad hoc scripting, while `uv`, `ruff`, `ty`, `UV_CACHE_DIR`, and the uv cache mount remain Python-profile-only.
  [check](verify:profiles.base-python-boundary)
- Rust profile can compile and run Rust projects
  [judge](../tests/judges/profiles.sh#test_rust_profile)
- Rust profile toolchain survives nixpkgs updates (no dynamic linker breakage)
  [judge](../tests/judges/profiles.sh#test_rust_profile_rebuild_stable)
- rust-analyzer can resolve the standard library (RUST_SRC_PATH is set correctly)
  [judge](../tests/judges/profiles.sh#test_rust_analyzer_sysroot)
- `wrix.rustProfile { toolchain = ./rust-toolchain.toml; sha256 = "..."; }` produces a working profile whose `toolchain` field is a fenix-combine derivation reflecting the file's component set
  [judge](../tests/judges/profiles.sh#test_rust_profile_constructor)
- `wrix.rustProfile { toolchain; sha256; packages = [p]; hostPackages = [h]; env = { K = "v"; }; runtimeSecrets = { TOKEN = "required"; }; mounts = [m]; networkAllowlist = [a]; }` lands extension args in the matching profile slots (package/mount/allowlist surfaces appended, env and runtime-secret attrsets right-merged)
  [check](verify:profiles.rust-extension-args)
- `wrix.rustProfile {}` (omitting required `toolchain`/`sha256`) errors at evaluation rather than silently producing an unpinned profile
  [check](verify:profiles.rust-required-args)
- Cargo registry/git mounts and the sccache cache parent are writable so cargo can fetch crates and sccache can cache artifacts without `Read-only file system` errors
  [judge](../tests/judges/profiles.sh#test_cargo_registry_writable)
- A profile mount with `optional = true` is preserved in `ProfileConfig` and omitted from launch planning when its expanded host source does not exist, on both supported platform paths
  [system](test-ci:test-optional-profile-mount)
- Python profile can run Python scripts with dependencies
  [judge](../tests/judges/profiles.sh#test_python_profile)
- uv cache mount is writable so uv can fetch packages not in the pre-warm set without `Read-only file system` errors
  [judge](../tests/judges/profiles.sh#test_uv_cache_writable)
- deriveProfile correctly merges image packages, host packages, and environment
  [judge](../tests/judges/profiles.sh#test_derive_profile_merge)
- A built-in profile's `corePackages` equals its `basePackages` floor plus its own toolchain, the base floor includes common build/SSH tools (`gnumake`, `openssh`), `rustProfile { toolchain = ./file }` includes the pinned toolchain in `corePackages`, and cargo-nextest remains rust leaf tooling rather than tier-1 core content
  [check](verify:profiles.core-membership)
- `deriveProfile p { packages = [extra]; }` appends `extra` to `.packages` but leaves `.corePackages` equal to `p.corePackages`, so `packages` − `corePackages` is exactly the downstream-added delta
  [check](verify:profiles.extra-packages-not-core)
- `deriveProfile p { packages = [image]; hostPackages = [host]; }` keeps image and host package extensions on their respective package surfaces without crossing either direction
  [check](verify:profiles.host-image-package-split)
- Profiles are composable (can extend extended profiles), and `runtimeSecrets` right-merges validated name-to-policy declarations while preserving built-in optional provider names
  [check](verify:profiles.nested-derive)
- `wrix.mkDevShell { profile = wrix.rustProfile { ... }; }` produces a devshell whose env contains an absolute `RUSTC` under `profile.toolchain`, `RUSTC_WRAPPER=sccache`, `SCCACHE_DIR`, `SCCACHE_CACHE_SIZE`, and `CARGO_INCREMENTAL=0` (the rust profile's `shellHook` was spliced)
  [check](verify:devshell.profile-shellhook-spliced)
- `wrix.mkDevShell { profile; packages = [extra]; }` shell has both `profile.hostPackages` and `extra` available on PATH, while image-only `profile.packages` stay out of the host PATH
  [check](verify:devshell.host-packages-source)
- `wrix.mkDevShell { profile; env = { K = "v"; }; }` shell has env var `K=v` (right-merge with profile.env, consumer wins on conflict)
  [check](verify:devshell.env-right-merge)
- `wrix.mkDevShell { profile; shellHook = "marker_xyz"; }` shell hook contains both `profile.shellHook` content AND `marker_xyz`, with the consumer hook firing **after** the profile's
  [check](verify:devshell.shellhook-order)
- Devshell constructors reject missing or ambiguous profile selection: `wrix.mkDevShell {}` without `profile` or `sandbox`, `wrix.mkDevShell { sandbox = ...; profile = ...; }`, and `sandbox.devShell { profile = ...; }` / `sandbox.devShell { sandbox = ...; }` all error at evaluation.
  [check](verify:devshell.profile-required)
- `wrix.mkDevShell { profile = ...; }` starts the workspace service container by default, exposes the project cache `file://` substituter/trusted key/post-build hook to host Nix, uses the `nixCache` publish/warm schema from `services.md`, and prints a suppressible reminder when the publish manifest is missing or stale; `nixCache = false` suppresses only the cache service, not beads
  [system](verify:devshell.nix-cache)
- `wrix.mkDevShell { profile = ...; }` with `.pre-commit-config.yaml` present sets `core.hooksPath` to the default hook bundle on entry
  [system](verify:devshell.prek-auto-set)
- `wrix.mkDevShell { profile = ...; }` without `.pre-commit-config.yaml` does NOT set `core.hooksPath` on entry
  [system](verify:devshell.prek-skip-absent-config)
- `wrix.mkDevShell { profile = ...; prekHooks = false; }` does NOT set `core.hooksPath` even when `.pre-commit-config.yaml` is present
  [system](verify:devshell.prek-opt-out)
- `wrix.mkDevShell { profile = ...; prekHooks = <custom-derivation>; }` sets `core.hooksPath` to the substituted derivation when `.pre-commit-config.yaml` is present
  [system](verify:devshell.prek-derivation-substitute)
- When `prekHooks` resolves to a derivation and a previous session left `core.hooksPath` set to a different store path, entering `mkDevShell` overwrites it and prints a one-line message naming the old value (covers both the `true` default case and the substituted-derivation case)
  [system](verify:devshell.prek-stale-config-overwrite)
- `wrix.mkDevShell { profile = ...; prekHooks = false; }` entered in a repo whose local git config already has `core.hooksPath` set leaves that value unchanged (passive opt-out preserves stale state per design)
  [system](verify:devshell.prek-opt-out-preserves-stale-config)
- The mkDevShell implementation contains no `prek install` invocation and no `chmod` on `.git/hooks`
  [check](verify:devshell.no-prek-install)
- `modules/flake/devshell.nix` does not set `core.hooksPath` (mkDevShell owns it)
  [check](verify:devshell.flake-module-does-not-own-hooks-path)
- On Linux hosts, a host devshell built via `wrix.mkDevShell { profile = wrix.rustProfile { toolchain; sha256; }; }` resolves `rustc` to the same `/nix/store/...` path as the sandbox built from the same profile; on Darwin the host and image toolchains share the pinned channel/version but resolve to platform-specific store paths per the Host/image toolchain split
  [judge](../tests/judges/profiles.sh#test_host_sandbox_rustc_same_store_path)
- `wrix.devToolchain` is not exposed by the lib (deleted; consumers reach `profile.toolchain`)
  [check](verify:profiles.no-dev-toolchain-lib)
- `profiles.rust.withToolchain` is not exposed on the rust profile attrset (replaced by top-level `wrix.rustProfile`)
  [check](verify:profiles.no-rust-with-toolchain)
- `profile.toolchain` is exposed on both `wrix.profiles.rust` and `wrix.rustProfile { toolchain; sha256; }`, and points at the same host-platform derivation `shellHook` interpolates into the PATH prepend (matches the image's toolchain in `profile.packages` on Linux hosts; diverges on Darwin per *Host/image toolchain split*)
  [judge](../tests/judges/profiles.sh#test_rust_toolchain_field)
- `wrix.profiles.rust` and `wrix.rustProfile { toolchain; sha256; }` closures contain zero `*-nightly-*` derivations after a fresh `nix flake update` (regression guard against reintroducing `fenix.packages.${system}.rust-analyzer`, which drags a nightly cargo/rustc/rust-std closure)
  [check](verify:profiles.rust-no-nightly-closure)
- `mkProfileImages { rust = …; }` produces a JSON file whose entry for `rust` is keyed by the image's selected agent and whose selected-agent entry has `ref`, `source`, `source_kind`, and `profile_config` fields, with `source` and `source_kind` resolving to the same image source path and source kind as the corresponding `(wrix.mkSandbox { profile = wrix.profiles.rust; agent = …; }).image`
  [check](test-ci:test-profile-images-manifest-shape)
- `packages.image-<name>[-<agent>]`, `packages.sandbox-<name>[-<agent>][-mcp]`, `packages.profile-images`, and `packages.profile-images-pi` all evaluate for each built-in profile, and `packages.default` resolves to `sandbox-rust-pi` with `meta.mainProgram = "wrix-run"`
  [check](verify:profiles.image-flake-outputs)
- `profiles.rust.buildPackage` is exposed and returns an attrset with `bin`, `clippy`, `nextest`, and `cargoArtifacts` fields
  [check](verify:profiles.rust-build-package-exposed)
- Editing a workspace source file changes the `bin` derivation hash but does **not** change the `cargoArtifacts` derivation hash (dep cache reused across edits)
  [check](verify:profiles.rust-build-package-workspace-edit-reuses-deps)
- Source filter excludes non-Cargo files: editing a `README.md` or other `*.md` file inside `src` does **not** change the `bin`, `clippy`, or `nextest` derivation hashes
  [check](verify:profiles.rust-build-package-source-filter-excludes-noncargo)
- Editing a `.rs` file invalidates `bin`, `clippy`, and `nextest` together (the workspace source closure is shared by all three) but does **not** invalidate `cargoArtifacts`
  [check](verify:profiles.rust-build-package-workspace-edit-skips-cargo-artifacts)
- Editing a file in `extraSrcs` invalidates `clippy` and `nextest` but does **not** invalidate `bin` or `cargoArtifacts`
  [check](verify:profiles.rust-build-package-extra-srcs-scoped-to-checks)
- `bin`, `clippy`, and `nextest` all close over `profile.toolchain`, so `${toolchain}/bin/rustc` resolves to the same `/nix/store/...` path across all three derivations, on both `wrix.profiles.rust` and `wrix.rustProfile { toolchain; sha256; }`
  [check](verify:profiles.rust-build-package-toolchain-alignment)
- `lib/mcp/tmux/mcp-server.nix` is a thin `wrix.profiles.rust.buildPackage` consumer (no direct `pkgs.rustPlatform.buildRustPackage` or `makeRustPlatform` call); `packages.tmux-mcp` consumes `.bin`; `tests/default.nix` exposes `tmux-mcp-clippy`, `tmux-mcp-nextest` checks
  [check](verify:profiles.rust-build-package-consumers-migrated)
- `modules/flake/devshell.nix` is a thin `sandbox.devShell { ... }` consumer (no hand-rolled `RUSTC`/`RUSTC_WRAPPER`/`SCCACHE_DIR`/`PATH` exports, no separate `profile.toolchain` entry in `packages`)
  [check](verify:devshell.flake-module-thin-consumer)
- Container entrypoints (`lib/sandbox/linux/entrypoint.sh`, `lib/sandbox/darwin/entrypoint.sh`) contain no rustup bootstrap logic — toolchain is baked into the image at build time
  [check](verify:profiles.sandbox-entrypoints-no-rustup)

## Requirements

### Functional

1. **Base Profile** — Core tools included in all environments, including a `python3` interpreter for stdlib-only ad hoc agent scripting
2. **Language Profiles** — Pre-configured Rust and Python environments
3. **Profile Extension** — `deriveProfile` API to extend existing profiles
4. **Package Bundling** — Profiles specify `packages` to include in the container image and `hostPackages` to include in host devshells. A profile also exposes `corePackages`, the wrix-controlled fixed-per-instance subset of image packages, so the image builder can layer wrix-default content separately from downstream additions (see `image-builder.md` § Provenance-Tiered Layering).
5. **Environment Configuration** — Profiles separate non-secret image environment defaults from runtime-secret name/policy declarations; secret values are launcher inputs, not profile data
6. **Mount Specifications** — Profiles can define default mounts (e.g., cargo cache)
7. **Toolchain Configuration** — Top-level `rustProfile { toolchain; sha256; ... }` constructor produces a project-pinned rust profile from a `rust-toolchain.toml`
8. **Rust Package Construction** — Rust profile exposes `buildPackage` for crane-backed Rust packages with split `bin`/`clippy`/`nextest` derivations
9. **Devshell Construction** — `sandbox.devShell { ... }` is the preferred host devshell entry point when a concrete sandbox exists, and top-level `mkDevShell { profile; ... }` remains available for profile-only shells; both consume `profile.hostPackages` for the host PATH and consumers do not splice `profile.shellHook` directly
10. **Prek Hook Management** — `mkDevShell` configures `core.hooksPath` from the hook derivation selected by `prekHooks` when `.pre-commit-config.yaml` is present, with `prekHooks = false` as the opt-out. The bundle's contents and shim behavior are owned by `specs/pre-commit.md`.
11. **Project Nix Cache Integration** — `mkDevShell` enables the `services.md` project cache by default for host pulls and host publishing of project-scoped Nix derivations; `nixCache = false` opts out.

### Non-Functional

1. **Curated Toolkit** — Base profile is a ready-to-work agent toolkit, not a minimal OS layer.
2. **Reproducible** — Same profile produces same environment via Nix

## Out of Scope

- Language-specific project scaffolding
- IDE configuration beyond Claude Code
- Auto-detection of `rust-toolchain.toml` at runtime (must be passed explicitly via `rustProfile`)
- Automatic pruning of cargo registry / git / uv caches — operators are expected to clean `~/.cargo/{registry,git}` and `~/.cache/uv` manually if they grow unbounded; sccache is self-capped via `SCCACHE_CACHE_SIZE`.
- Tracking nightly rust-analyzer in the default profile. Building `fenix.packages.${system}.rust-analyzer` from source pulls a matching nightly cargo/rustc/rust-std closure into every consumer's flake on each input update; the profile pins `fenix.stable.rust-analyzer-preview` instead. Consumers who need nightly RA opt in via `deriveProfile`.
- Cross-compilation in `buildPackage`. Always builds for `pkgs.stdenv.hostPlatform.system`. Consumers needing cross builds drop down to crane directly.
- Returning multiple `bin` outputs from a single `buildPackage` call. Workspaces with multiple binary crates call `buildPackage` once per binary, threading the same `cargoArtifacts` through to share dep compilation.
