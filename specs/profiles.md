# Profiles System

Pre-configured development environments with language-specific toolchains.

## Problem Statement

Every project benefits from a shared agent-tooling floor, while language
projects also need a coherent toolchain and cache configuration. Profiles model
both the Linux image surface and the host devshell surface so consumers can
extend either without rebuilding the bundle by hand or mixing platform
packages.

## Architecture

A profile is a Nix attrset consumed by two peers. `mkSandbox` uses the image
package and environment surfaces to produce an image and launcher;
`mkDevShell` uses the host surfaces to produce a native shell. The Rust profile
also carries a host toolchain derivation and package builder. On Linux, the
host and image toolchains are one derivation. On Darwin, they use the same
channel and components but different platform store paths.

Built-in profiles are constants under `profiles.<name>`. `deriveProfile`
extends any profile. `rustProfile { toolchain; sha256; ... }` constructs a
project-pinned Rust profile. Profiles do not expose per-profile builder methods
such as `withToolchain`.

Workspace-service and project-cache behavior belongs to `services.md`.
Devshell hook-bundle selection is described below, while `pre-commit.md` owns
the bundle and hook behavior. `cli.md` owns durable repository initialization.
Image layer assignment belongs to `image-builder.md`.

## Profile Attrset Schema

| Field | Type | Purpose |
|-------|------|---------|
| `name` | string | Profile identifier |
| `packages` | derivation list | Linux image packages |
| `hostPackages` | derivation list | Host-native devshell packages |
| `corePackages` | derivation list | Wrix-controlled fixed subset of `packages`; constructors set it and extensions preserve it. `image-builder.md` owns how this subset affects image layering. |
| `env` | string attrset | Non-secret image environment defaults |
| `hostEnv` | string attrset | Non-secret host defaults |
| `runtimeSecrets` | policy attrset | Validated environment name to `"optional"` or `"required"`; values are runtime-only |
| `mounts` | mount list | `{ source, dest, mode, optional }` bind declarations |
| `networkAllowlist` | string list | Domains added in `WRIX_NETWORK=limit` |
| `enabledPlugins` | attrset | Claude plugin enablement |
| `shellHook` | shell snippet | Internal host-alignment hook consumed by `mkDevShell` |
| `writableDirs` | string list | Linux tmpfs parents that remain writable when optional mounts are absent |

An optional mount is omitted when its expanded host source does not exist.
`runtimeSecrets` stores declarations only; `security.md` owns runtime delivery
and redaction. This spec owns the built-in declaration names listed under the
Base Profile.

`deriveProfile` concatenates `packages`, `hostPackages`, `mounts`, and
`networkAllowlist` on their respective surfaces. `env` right-merges into both
image and host defaults; `hostEnv` then overrides only the host. Runtime-secret
declarations right-merge after validating names and policies. `corePackages`
passes through unchanged. Other fields use the extension value when supplied
and otherwise retain the base value.

## Built-in Profiles

### Base Profile

The base image package set is exact; additions require updating its membership
verifier.

| Category | Image packages |
|----------|----------------|
| Shell + POSIX core | bash, coreutils, diffutils, findutils, gawk, getent, gnugrep, gnused, gnutar, gnumake, gzip, less, patch, rsync, tree, unzip, util-linux, whichQuiet, zip |
| File + text | fd, file, ripgrep, sqlite, vim |
| Network + process | curl, iproute2, nftables, iptables, libcap, iputils, lsof, netcat, openssh, procps |
| Data + scripting | jq, python3, yq |
| Package manager | nix |
| VCS | git, gh |
| Issue tracking | beads (`bd`), dolt |
| Agent tooling | man, prek, shellcheck, tmux, treefmt |

The host package set is the host-native subset: it omits the Linux-only image
network/process packages, getent, and the image treefmt wrapper, and supplies a
host `whichQuiet` wrapper. `mkSandbox` adds the Linux `wrix` CLI separately.

All built-in profiles set `BD_DISABLE_METRICS=1` in their image and host
environments, disabling Beads usage metrics in sandboxes and devshells even
when the saved user preference enables them
[check](verify:profiles.beads-metrics-disabled).

The base profile has no mounts. It declares `ANTHROPIC_API_KEY`,
`CLAUDE_CODE_OAUTH_TOKEN`, and `OPENAI_API_KEY` as optional runtime secrets. Its
limit-mode allowlist is `api.anthropic.com`, `github.com`, `ssh.github.com`, and
`cache.nixos.org`.

`whichQuiet` suppresses unsuccessful lookup noise. `treefmt` is the project
formatter wrapper. Host `~/.claude` is not mounted; project settings remain
under the workspace.

### Rust Profile

The Rust profile uses fenix derivations rather than runtime rustup downloads.
The default combines `stable.defaultToolchain`, stable `rust-src`, and stable
`rust-analyzer-preview`. This keeps the default closure free of fenix's
nightly source-built rust-analyzer toolchain.

The fixed Rust additions in `corePackages` are the selected fenix toolchain,
`gcc`, both OpenSSL runtime and development outputs, `pkg-config`,
`postgresql.lib`, and `sccache`. `cargo-nextest` is the sole built-in Rust
package outside `corePackages`. The host surface contains host-native
counterparts of all Rust additions.

The image and host environments align these values with their platform's
selected derivations:

| Variable | Contract |
|----------|----------|
| `RUSTC` | Absolute selected compiler path |
| `RUST_SRC_PATH` | Selected standard-library source |
| `RUSTC_WRAPPER`, `CARGO_BUILD_RUSTC_WRAPPER` | sccache binary |
| `SCCACHE_DIR` | `/home/wrix/.cache/sccache` in the image; `$HOME/.cache/sccache` default in a host shell |
| `SCCACHE_CACHE_SIZE` | `50G` |
| `CARGO_INCREMENTAL` | `0` |
| `LIBRARY_PATH`, `OPENSSL_INCLUDE_DIR`, `OPENSSL_LIB_DIR` | Platform-matching library paths |

`CARGO_HOME` and `CARGO_TARGET_DIR` remain unset. The profile optionally mounts
`~/.cargo/registry`, `~/.cargo/git`, and `~/.cache/sccache` read-write at their
`/home/wrix` counterparts. `/home/wrix/.cargo` and `/home/wrix/.cache` are
writable parents. The Rust allowlist adds `crates.io`, `static.crates.io`, and
`index.crates.io`.

`profile.toolchain` is the host-platform combined fenix derivation used by the
devshell and `buildPackage`. It matches the image toolchain on Linux. Sibling
host applications that run Cargo use this field in `runtimeInputs` rather than
instantiating fenix again.

#### Rust package builder

`profile.buildPackage` is a crane-backed builder with this public signature:

```nix
profile.buildPackage {
  src;
  cargoLock;
  extraSrcs ? { };
  cargoArtifacts ? null;
  cargoExtraArgs ? "";
  buildInputs ? [ ];
  propagatedBuildInputs ? [ ];
  nativeBuildInputs ? [ ];
  meta ? { };
  srcFilter ? null;
}
=> { bin; clippy; nextest; cargoArtifacts; }
```

`bin`, `clippy`, and `nextest` are independent derivations that use the selected
host toolchain. Only the check outputs see `extraSrcs`; editing those inputs
does not invalidate `bin` or `cargoArtifacts`. The default source filter uses
crane's Cargo-source filter, while `srcFilter` replaces it for projects with
compile-time non-Rust inputs. `cargoArtifacts` defaults to a dependency build
and can be shared across calls. Each call returns one binary package and
builds for the host platform without cross-compilation.

The builder is Nix-sandboxed. Interactive host/image compilation uses sccache;
package derivations use `cargoArtifacts`. On Darwin, image cache mounts are
startup pre-warms because the platform staging path does not synchronize
container writes back to the host.

### Python Profile

Python inherits `python3` from base and adds `uv`, `ruff`, and `ty` to both
package surfaces. It sets `UV_CACHE_DIR=/home/wrix/.cache/uv`, optionally
mounts `~/.cache/uv` there read-write, and adds `pypi.org` and
`files.pythonhosted.org` to the limit-mode allowlist.

## Public API

### deriveProfile

```nix
wrix.deriveProfile baseProfile {
  packages ? [ ];
  hostPackages ? [ ];
  env ? { };
  hostEnv ? { };
  runtimeSecrets ? { };
  mounts ? [ ];
  networkAllowlist ? [ ];
  # Other profile fields may be replaced explicitly.
}
```

Callers name image and host package additions independently. Nested derivation
uses the same merge rules.

### rustProfile

```nix
wrix.rustProfile {
  toolchain;
  sha256;
  packages ? [ ];
  hostPackages ? [ ];
  env ? { };
  hostEnv ? { };
  runtimeSecrets ? { };
  mounts ? [ ];
  networkAllowlist ? [ ];
}
```

`toolchain` and `sha256` are required. The constructor reads a
`rust-toolchain.toml`, resolves platform-specific fenix sets, combines stable
`rust-src` and `rust-analyzer-preview`, and applies the same extension rules as
`deriveProfile`. Toolchain files omit `rust-analyzer` because the constructor
adds it. A different analyzer can be appended through `deriveProfile`.

A typical shared consumer binds one constructed profile to both surfaces:

```nix
let
  profile = wrix.rustProfile {
    toolchain = ./rust-toolchain.toml;
    sha256 = "sha256-...";
  };
  sandbox = wrix.mkSandbox { inherit profile; };
in {
  devShells.default = sandbox.devShell { };
  packages.image = sandbox.image.source;
}
```

### mkDevShell

`sandbox.devShell { ... }` binds a concrete sandbox's profile and configured
launcher. `wrix.mkDevShell` is the profile-only form:

```nix
wrix.mkDevShell {
  profile = profile;       # exactly one of profile or sandbox
  packages ? [ ];
  env ? { };
  shellHook ? "";
  prekHooks ? true;
  nixCache ? true;
}
```

Exactly one of `profile` or `sandbox` is accepted. A bound
`sandbox.devShell` does not accept either override. Composition order is:

| Field | Rule |
|-------|------|
| `packages` | `profile.hostPackages ++ packages` |
| `env` | `profile.hostEnv // env` |
| `shellHook` | internal lifecycle, then `profile.shellHook`, then consumer hook |

The lifecycle runs workspace services, project-cache integration, and hook-path
selection before the profile and consumer hooks. The cache schema belongs to
`services.md`.

#### Prek hook management

When `.pre-commit-config.yaml` exists and `prekHooks` resolves to a derivation,
the lifecycle sets local `core.hooksPath` to that derivation on every shell
entry. `true` selects the default bundle, `false` performs no hook-path action,
and a derivation substitutes the bundle. An existing different path is
overwritten with a one-line notice. Opting out leaves any existing value
unchanged. `pre-commit.md` owns bundle contents and hook semantics.

## Profile-Image Manifest

`wrix.lib.${system}.mkProfileImages` accepts profile names mapped to the
`.image` values returned by `mkSandbox`. It produces JSON keyed first by
profile name and then by the image's selected agent. Each agent entry contains
`{ ref, source, source_kind, profile_config }`; the image metadata is copied
opaquely from the selected image. `image-builder.md` owns and verifies source
kind and composition; `sandbox.md` owns agent selection.

Bundled `packages.profile-images` covers direct images and
`packages.profile-images-pi` covers the Pi images used by the repository
runtime. External flakes call `mkProfileImages` for custom profiles.

## Flake Outputs

For each built-in `base`, `rust`, and `python` profile, the flake exposes:

| Family | Contract |
|--------|----------|
| `packages.image-<profile>[-<agent>]` | Selected OCI image source; the unsuffixed image is direct |
| `packages.sandbox-<profile>[-<agent>]` | Runnable configured launcher; the unsuffixed launcher is direct |
| `packages.sandbox-<profile>[-<agent>]-mcp` | Same agent/profile with runtime MCP selection |
| `packages.profile-images` | Direct profile manifest |

Agent suffixes are `-claude` and `-pi`. `packages.default` is the Rust Pi
sandbox and runs through `wrix-run`. MCP selection is orthogonal to profiles;
there are no per-server profile variants.

## Success Criteria

- Base profile provides functional development environment
  [judge](../tests/judges/profiles.sh#test_base_profile_functional)
- Base profile exposes `python3` on both image and host package surfaces for stdlib-only ad hoc scripting, while `uv`, `ruff`, `ty`, `UV_CACHE_DIR`, and the uv cache mount remain Python-profile-only.
  [check](verify:profiles.base-python-boundary)
- Rust profile can compile and run a Cargo project with the toolchain and host package surface it exposes
  [check](verify:profiles.rust-compile-run)
- Rust profile toolchain survives nixpkgs updates (no dynamic linker breakage)
  [judge](../tests/judges/profiles.sh#test_rust_profile_rebuild_stable)
- rust-analyzer can resolve the standard library (RUST_SRC_PATH is set correctly)
  [judge](../tests/judges/profiles.sh#test_rust_analyzer_sysroot)
- `wrix.rustProfile { toolchain = ./rust-toolchain.toml; sha256 = "..."; }` produces a working profile whose `toolchain` field is a fenix-combine derivation reflecting the file's component set
  [judge](../tests/judges/profiles.sh#test_rust_profile_constructor)
- `wrix.rustProfile { toolchain; sha256; packages = [p]; hostPackages = [h]; env = { K = "v"; }; hostEnv = { H = "v"; }; runtimeSecrets = { TOKEN = "required"; }; mounts = [m]; networkAllowlist = [a]; }` lands extension args in the matching profile slots (package/mount/allowlist surfaces appended; environment and runtime-secret attrsets right-merged)
  [check](verify:profiles.rust-extension-args)
- `wrix.rustProfile {}` (omitting required `toolchain`/`sha256`) errors at evaluation rather than silently producing an unpinned profile
  [check](verify:profiles.rust-required-args)
- Cargo registry/git mounts and the sccache cache parent are writable so cargo can fetch crates and sccache can cache artifacts without `Read-only file system` errors
  [judge](../tests/judges/profiles.sh#test_cargo_registry_writable)
- A profile mount with `optional = true` is preserved in the Nix-generated `ProfileConfig`
  [check](test-ci:test-profile-config-wrapper)
- Both Linux and Darwin launch planners omit an optional profile mount when its expanded host source does not exist
  [test](command::launch::test::missing_optional_profile_mount_is_skipped_by_platform_planners)
- Python profile can run Python scripts with dependencies
  [judge](../tests/judges/profiles.sh#test_python_profile)
- uv cache mount is writable so uv can fetch packages not in the pre-warm set without `Read-only file system` errors
  [judge](../tests/judges/profiles.sh#test_uv_cache_writable)
- deriveProfile correctly merges image packages, host packages, and environment
  [judge](../tests/judges/profiles.sh#test_derive_profile_merge)
- Built-in `corePackages` and `packages` equal the exact documented base, Rust, and Python package sets; a pinned Rust constructor substitutes its selected toolchain in `corePackages`; cargo-nextest is the only built-in Rust package outside `corePackages`
  [check](verify:profiles.core-membership)
- `deriveProfile p { packages = [extra]; }` appends `extra` to `.packages` but leaves `.corePackages` equal to `p.corePackages`, so `packages` − `corePackages` is exactly the downstream-added delta
  [check](verify:profiles.extra-packages-not-core)
- `deriveProfile p { packages = [image]; hostPackages = [host]; }` keeps image and host package extensions on their respective package surfaces without crossing either direction
  [check](verify:profiles.host-image-package-split)
- Profiles are composable (can extend extended profiles); `env` right-merges into image and host surfaces, `hostEnv` may override the host surface, and `runtimeSecrets` right-merges declarations while preserving built-in optional provider names
  [check](verify:profiles.nested-derive)
- `deriveProfile` and `rustProfile` accept valid runtime-secret declarations and reject malformed environment names or policies other than `"optional"` and `"required"` at profile construction
  [check](verify:profiles.runtime-secret-validation)
- `wrix.mkDevShell { profile = wrix.rustProfile { ... }; }` produces a devshell whose env contains an absolute `RUSTC` under `profile.toolchain`, `RUSTC_WRAPPER=sccache`, `SCCACHE_DIR`, `SCCACHE_CACHE_SIZE`, and `CARGO_INCREMENTAL=0` (the rust profile's `shellHook` was spliced)
  [check](verify:devshell.profile-shellhook-spliced)
- The rust devshell exports host-platform `RUSTC`, `RUST_SRC_PATH`, `LIBRARY_PATH`, and `OPENSSL_*` paths; on Darwin they differ from the Linux image paths in `profile.env`
  [check](verify:devshell.rust-host-env)
- `wrix.mkDevShell { profile; packages = [extra]; }` shell has both `profile.hostPackages` and `extra` available on PATH, while image-only `profile.packages` stay out of the host PATH
  [check](verify:devshell.host-packages-source)
- `wrix.mkDevShell { profile; env = { K = "v"; }; }` shell has env var `K=v` (right-merge with `profile.hostEnv`, consumer wins on conflict)
  [check](verify:devshell.env-right-merge)
- `wrix.mkDevShell { profile; shellHook = "marker_xyz"; }` shell hook contains both `profile.shellHook` content AND `marker_xyz`, with the consumer hook firing **after** the profile's
  [check](verify:devshell.shellhook-order)
- Devshell constructors reject missing or ambiguous profile selection: `wrix.mkDevShell {}` without `profile` or `sandbox`, `wrix.mkDevShell { sandbox = ...; profile = ...; }`, and `sandbox.devShell { profile = ...; }` / `sandbox.devShell { sandbox = ...; }` all error at evaluation.
  [check](verify:devshell.profile-required)
- `wrix.mkDevShell { profile = ...; }` places the `pre-push-checks` and `skip-if-missing` wrappers on the host devshell PATH
  [check](verify:prek.wrappers-on-devshell-path)
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
- Flake-level devshell assembly does not set `core.hooksPath`; `mkDevShell` owns that lifecycle state
  [check](verify:devshell.flake-module-does-not-own-hooks-path)
- On Linux hosts, a host devshell built via `wrix.mkDevShell { profile = wrix.rustProfile { toolchain; sha256; }; }` resolves `rustc` to the same `/nix/store/...` path as the sandbox built from the same profile; on Darwin the host and image toolchains share the pinned channel/version but resolve to the platform-specific store paths described in Architecture
  [judge](../tests/judges/profiles.sh#test_host_sandbox_rustc_same_store_path)
- The public lib exposes toolchain identity through `profile.toolchain`, not `wrix.devToolchain`
  [check](verify:profiles.no-dev-toolchain-lib)
- Project-pinned profiles use top-level `wrix.rustProfile`; `profiles.rust.withToolchain` is not part of the profile attrset
  [check](verify:profiles.no-rust-with-toolchain)
- `profile.toolchain` is exposed on both `wrix.profiles.rust` and `wrix.rustProfile { toolchain; sha256; }`, and points at the same host-platform derivation `shellHook` interpolates into the PATH prepend (matches the image's toolchain in `profile.packages` on Linux hosts; uses the Darwin host counterpart on Darwin)
  [judge](../tests/judges/profiles.sh#test_rust_toolchain_field)
- `wrix.profiles.rust` and `wrix.rustProfile { toolchain; sha256; }` closures contain zero `*-nightly-*` derivations after a fresh `nix flake update` (regression guard against reintroducing `fenix.packages.${system}.rust-analyzer`, which drags a nightly cargo/rustc/rust-std closure)
  [check](verify:profiles.rust-no-nightly-closure)
- `mkProfileImages { rust = …; }` produces a JSON file whose entry for `rust` is keyed by the image's selected agent and whose selected-agent entry has `ref`, `source`, `source_kind`, and `profile_config` fields. Image metadata is copied opaquely from the corresponding `(wrix.mkSandbox { profile = wrix.profiles.rust; agent = …; }).image`; its values and meanings are owned and verified by `image-builder.md`
  [check](test-ci:test-profile-images-manifest-shape)
- `packages.image-<name>[-<agent>]` resolves to the matching sandbox's selected `.image.source`; source metadata remains owned by `image-builder.md`. All sandbox and profile-manifest outputs evaluate for each built-in profile, and `packages.default` resolves to `sandbox-rust-pi` with `meta.mainProgram = "wrix-run"`
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
- Cargo selects `${profile.toolchain}/bin/rustc` while building `bin`, `clippy`, and `nextest` for both `wrix.profiles.rust` and `wrix.rustProfile { toolchain; sha256; }`
  [check](verify:profiles.rust-build-package-toolchain-alignment)
- The tmux MCP package depends on the Rust profile's `buildPackage` boundary: its runtime package consumes `bin`, while `clippy` and `nextest` remain independent checks
  [check](verify:profiles.rust-build-package-consumer-boundary)
- The repository devshell depends on the sandbox-owned `devShell` constructor rather than reconstructing profile toolchain or environment state
  [check](verify:devshell.sandbox-boundary)
- Platform container entrypoints contain no rustup bootstrap logic; the Rust toolchain is image-built
  [check](verify:profiles.sandbox-entrypoints-no-rustup)

## Requirements

### Functional

1. **Base Profile** — Image and host surfaces provide their documented core-tool sets, including `python3` for stdlib-only ad hoc agent scripting
2. **Language Profiles** — Pre-configured Rust and Python environments
3. **Profile Extension** — `deriveProfile` API to extend existing profiles
4. **Package Bundling** — Profiles specify `packages` to include in the container image and `hostPackages` to include in host devshells. A profile also exposes `corePackages`, the wrix-controlled fixed-per-instance subset of image packages, so the image builder can layer wrix-default content separately from downstream additions (see `image-builder.md` § Provenance-Tiered Layering).
5. **Environment Configuration** — Profiles separate non-secret image (`env`) and host (`hostEnv`) defaults from runtime-secret name/policy declarations; secret values are launcher inputs, not profile data
6. **Mount Specifications** — Profiles can define default mounts (e.g., cargo cache)
7. **Toolchain Configuration** — Top-level `rustProfile { toolchain; sha256; ... }` constructor produces a project-pinned rust profile from a `rust-toolchain.toml`
8. **Rust Package Construction** — Rust profile exposes `buildPackage` for crane-backed Rust packages with split `bin`/`clippy`/`nextest` derivations
9. **Devshell Construction** — `sandbox.devShell { ... }` is the preferred host devshell entry point when a concrete sandbox exists, and top-level `mkDevShell { profile; ... }` remains available for profile-only shells; both consume `profile.hostPackages` for the host PATH and consumers do not splice `profile.shellHook` directly
10. **Prek Hook Management** — `mkDevShell` configures `core.hooksPath` from the hook derivation selected by `prekHooks` when `.pre-commit-config.yaml` is present, with `prekHooks = false` as the opt-out. The bundle's contents and shim behavior are owned by `specs/pre-commit.md`.

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
