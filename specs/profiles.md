# Profiles System

Pre-configured development environments with language-specific toolchains.

## Problem Statement

Every project benefits from a shared agent-tooling floor, while language
projects also need a coherent toolchain and cache configuration. Profiles model
both the Linux image surface and the host devshell surface so consumers can
extend either without rebuilding the bundle by hand or mixing platform packages.

## Architecture

A profile is a Nix attrset consumed by two peers. `mkSandbox` uses the image
package and environment surfaces to produce image/configuration data;
`mkDevShell` uses the host surfaces to produce a native shell. The Rust profile
also carries a host toolchain derivation and package builder. On Linux, the host
and image toolchains are one derivation. On Darwin, they use the same channel
and components but different platform store paths.

Built-in profiles are constants under `profiles.<name>`. `deriveProfile` extends
any profile. `rustProfile { toolchain; sha256; ... }` constructs a
project-pinned Rust profile. Profiles do not expose per-profile builder methods
such as `withToolchain`.

Host-service behavior belongs to `services.md`; project-cache policy and
integration belong to `cache.md`. Devshell hook-bundle selection is described
below, while `pre-commit.md` owns the bundle and hook behavior. `cli.md` owns
durable repository initialization. Image layer assignment belongs to
`image-builder.md`.

## Profile Attrset Schema

| Field              | Type            | Purpose                                                                                                                                                     |
| ------------------ | --------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `name`             | string          | Profile identifier                                                                                                                                          |
| `packages`         | derivation list | Linux image packages                                                                                                                                        |
| `hostPackages`     | derivation list | Host-native devshell packages                                                                                                                               |
| `corePackages`     | derivation list | Wrix-controlled fixed subset of `packages`; constructors set it and extensions preserve it. `image-builder.md` owns how this subset affects image layering. |
| `env`              | string attrset  | Non-secret image environment defaults                                                                                                                       |
| `hostEnv`          | string attrset  | Non-secret host defaults                                                                                                                                    |
| `runtimeSecrets`   | policy attrset  | Validated environment name to `"optional"` or `"required"`; values are runtime-only                                                                         |
| `mounts`           | mount list      | `{ source, dest, mode, optional }` bind declarations                                                                                                        |
| `networkAllowlist` | rule list       | Explicit host/port rules added in `WRIX_NETWORK=limit`; see [Network Allowlist](#network-allowlist)                                                         |
| `enabledPlugins`   | attrset         | Claude plugin enablement                                                                                                                                    |
| `shellHook`        | shell snippet   | Internal host-alignment hook consumed by `mkDevShell`                                                                                                       |
| `writableDirs`     | string list     | Linux tmpfs parents that remain writable when optional mounts are absent                                                                                    |

An optional mount is omitted when its expanded host source does not exist.
`runtimeSecrets` stores declarations only; `security.md` owns runtime delivery
and redaction. This spec owns the built-in declaration names listed under the
Base Profile.

`deriveProfile` concatenates `packages`, `hostPackages`, `mounts`, and
`networkAllowlist` on their respective surfaces. `env` right-merges into both
image and host defaults; `hostEnv` then overrides only the host. Runtime-secret
declarations right-merge after validating names and policies. `corePackages`
passes through unchanged. Other fields use the extension value when supplied and
otherwise retain the base value.

## Network Allowlist

Acceptance: [Network Allowlist Acceptance](#network-allowlist-acceptance).

`networkAllowlist` is a list of structured rules with exactly `host` and `ports`
fields. Each host is a nonempty string; ports are a nonempty list of integer TCP
destination ports in the range 1–65535. For example:

```nix
networkAllowlist = [
  { host = "github.com"; ports = [ 22 443 ]; }
  { host = "registry.example.org"; ports = [ 8443 ]; }
];
```

The rules retain these object fields in generated JSON. Hostname-only entries,
`"host:port"` shorthand, missing/empty ports and unknown rule fields are errors;
there are no implicit ports, all-port defaults or compatibility aliases. Profile
constructors validate rules at Nix evaluation. Rust parsing independently
enforces the owned wire boundary described by `sandbox.md`.

Base, language-profile and consumer rules compose through the existing list
concatenation semantics. Admission matches a listed host/port pair;
concatenation does not introduce ports that no constituent rule listed.
`proxy.md` owns request admission, resolution and enforcement limitations, not
profile membership.

## Built-in Profiles

### Base Profile

The base image package set is exact; additions require updating its membership
verifier.

| Category           | Image packages                                                                                                                                            |
| ------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Shell + POSIX core | bash, coreutils, diffutils, findutils, gawk, getent, gnugrep, gnused, gnutar, gnumake, gzip, less, patch, rsync, tree, unzip, util-linux, whichQuiet, zip |
| File + text        | fd, file, ripgrep, sqlite, vim                                                                                                                            |
| Network + process  | curl, iproute2, nftables, iptables, libcap, iputils, lsof, netcat, openssh, procps                                                                        |
| Data + scripting   | jq, python3, yq                                                                                                                                           |
| Package manager    | nix                                                                                                                                                       |
| VCS                | git, gh                                                                                                                                                   |
| Issue tracking     | beads (`bd`), dolt                                                                                                                                        |
| Agent tooling      | man, prek, shellcheck, tmux, treefmt                                                                                                                      |

The host package set is the host-native subset: it omits the Linux-only image
network/process packages, getent, and the image treefmt wrapper, and supplies a
host `whichQuiet` wrapper. `mkSandbox` adds the Linux `wrix` CLI separately.

All built-in profiles set `BD_DISABLE_METRICS=1` in their image and host
environments, disabling Beads usage metrics in sandboxes and devshells even when
the saved user preference enables them
[check](verify:profiles.beads-metrics-disabled).

The base profile has no mounts. It declares `ANTHROPIC_API_KEY`,
`CLAUDE_CODE_OAUTH_TOKEN`, and `OPENAI_API_KEY` as optional runtime secrets. Its
limit-mode rules are:

| Host                | Explicit TCP ports |
| ------------------- | ------------------ |
| `api.anthropic.com` | 443                |
| `github.com`        | 22, 443            |
| `ssh.github.com`    | 443                |
| `cache.nixos.org`   | 443                |

Membership and ports are covered by
[Network Allowlist Acceptance](#network-allowlist-acceptance).

`whichQuiet` suppresses unsuccessful lookup noise. `treefmt` is the project
formatter wrapper. Host `~/.claude` is not mounted; project settings remain
under the workspace.

### Rust Profile

The Rust profile uses fenix derivations rather than runtime rustup downloads.
The default combines `stable.defaultToolchain`, stable `rust-src`, and stable
`rust-analyzer-preview`. This keeps the default closure free of fenix's nightly
source-built rust-analyzer toolchain.

The fixed Rust additions in `corePackages` are the selected fenix toolchain,
`gcc`, both OpenSSL runtime and development outputs, `pkg-config`,
`postgresql.lib`, and `sccache`. `cargo-nextest` is the sole built-in Rust
package outside `corePackages`. The host surface contains host-native
counterparts of all Rust additions.

The image and host environments align these values with their platform's
selected derivations:

| Variable                                                 | Contract                                                                                                        |
| -------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- |
| `RUSTC`                                                  | Absolute selected compiler path                                                                                 |
| `RUST_SRC_PATH`                                          | Selected standard-library source                                                                                |
| `RUSTC_WRAPPER`, `CARGO_BUILD_RUSTC_WRAPPER`             | sccache binary                                                                                                  |
| `SCCACHE_DIR`                                            | `/home/wrix/.cache/sccache` in the image; `$HOME/.cache/sccache` default in a host shell                        |
| `SCCACHE_CACHE_SIZE`                                     | `50G` for host/private local caches; shared capacity belongs to [Services](services.md#shared-compiler-service) |
| `CARGO_INCREMENTAL`                                      | Unset; Cargo retains its development/test incremental defaults                                                  |
| `LIBRARY_PATH`, `OPENSSL_INCLUDE_DIR`, `OPENSSL_LIB_DIR` | Platform-matching library paths                                                                                 |

`CARGO_INCREMENTAL`, `CARGO_HOME`, and `CARGO_TARGET_DIR` remain unset. Cargo
can incrementally rebuild workspace development/test crates while sccache caches
eligible dependency compilations. Explicit `CARGO_INCREMENTAL=0` remains a
caller override; forcing `1` is incompatible with the packaged sccache wrapper.
Cargo registry/git and sccache use the sandbox-only locations and backing
defined by [Tool Cache Data Boundary](#tool-cache-data-boundary), not personal
host caches. `/home/wrix/.cargo` and `/home/wrix/.cache` remain writable
parents. The Rust allowlist adds explicit port 443 rules for `crates.io`,
`static.crates.io`, and `index.crates.io`; see
[Network Allowlist Acceptance](#network-allowlist-acceptance).

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
and can be shared across calls. Each call returns one binary package and builds
for the host platform without cross-compilation.

The builder is Nix-sandboxed. Interactive host/image compilation uses separate
sccache data under [Tool Cache Data Boundary](#tool-cache-data-boundary);
package derivations use `cargoArtifacts`. General Darwin directory staging does
not imply managed-cache persistence; no personal-host prewarm is used.

### Python Profile

Python inherits `python3` from base and adds `uv`, `ruff`, and `ty` to both
package surfaces. Its image sets `UV_CACHE_DIR=/home/wrix/.cache/uv` with
sandbox-only backing under
[Tool Cache Data Boundary](#tool-cache-data-boundary), not the personal host uv
cache. It adds explicit port 443 rules for `pypi.org` and
`files.pythonhosted.org` to the limit-mode allowlist; see
[Network Allowlist Acceptance](#network-allowlist-acceptance).

## Tool Cache Data Boundary

Acceptance: [Tool Cache Isolation Acceptance](#tool-cache-isolation-acceptance),
[Tool Cache Sharing Acceptance](#tool-cache-sharing-acceptance) and
[Shared Compiler Acceptance](#shared-compiler-acceptance).

Built-in Rust Cargo registry/git and sccache data, and Python uv cache data, use
Wrix-managed sandbox-only storage. They are not bind mounts, snapshots or
prewarm copies of the invoking user's personal host caches, and sandbox writes
are not synchronized back into those caches. Host tool-cache settings and data
remain independent. Empty managed caches are not permission to fall back to
personal host data.

Worker-local tool locations remain `/home/wrix/.cargo/registry`,
`/home/wrix/.cargo/git`, `/home/wrix/.cache/sccache` and `/home/wrix/.cache/uv`
for their respective profiles, with writable parents and cache directories.
Shared Rust uses the service-owned sccache data through
[Compiler Delegation](#shared-rust-compiler-service), not a shared raw directory
behind independent worker daemons. Ordinary permitted tool fetches can populate
cold caches; this does not broaden network or credential grants. Host-cache
contents are not automatically imported or migrated. Initial cold-cache costs
are accepted.

### Shared and Private Tool Cache Modes

Acceptance: [Tool Cache Sharing Acceptance](#tool-cache-sharing-acceptance).

The managed tool caches have one all-or-nothing sharing control for a launch. It
can be selected for one invocation or remembered as optional repository-root
`wrix.toml` policy. Both run and spawn resolve it from explicit launch choice,
then the selected workspace's repository preference, then shared mode. Omission
inherits; an explicit choice can enable or disable sharing regardless of the
repository preference. Policy is read anew for each launch; outside a repository
there is no repository tier.

A one-off choice does not rewrite/persist repository policy or create a file to
record defaults. Repository policy is a workspace-editable preference, not an
agent-resistant host restriction. This control has no additional environment
policy input and is independent of Nix-cache enablement/publication policy.
[CLI Tool Cache Controls](cli.md#tool-cache-controls) defines the remembered
`tool_caches.shared` boolean;
[Launch Selection](sandbox.md#launch-tool-cache-selection) uses short run flags
`--cache` / `--no-cache` or the equivalent optional spawn boolean. These names
do not make the tool-cache control a Nix-cache switch.

The resolved modes are:

- **Shared (default):** workers of this host user reuse one sandbox-only pool
  across repositories and Loom clones, rather than retaining separate
  repository-sized pools. Applicable Cargo registry/git, sccache and uv data
  participate together. Updates must support subsequent worker reuse; disposable
  startup copies without retained updates do not satisfy sharing. Concurrent use
  must preserve normal tool correctness.
- **Private (sharing off):** every applicable managed cache uses fresh private
  writable data for that launch. It neither reads nor writes the shared pool,
  including through prewarm copies, and has no personal-host fallback. The
  private data is disposable with the sandbox, not a retained per-repository
  pool. Ordinary tool use still works within existing network/credential grants.

Shared mode deliberately accepts cached-data visibility and artifact influence
between workers across repositories, including launches with different
credential grants. Private Git sources, packages and compiled contents can enter
this pool; native cache keys/checksums/locks are not repository authorization or
correct-build attestation. Dedicated runtime credential/configuration files and
management authority are not pooled as cache data.

Use the coarse private mode for workloads whose tool-cache contents must not
join this worker trust domain. Switching modes does not migrate private data
into the pool or erase/recall previously shared bytes. This controls built-in
cache data flow and compiler delegation, not sanitization of arbitrary workspace
contents or other explicit mounts. Selective directory/Git-source exclusions and
confidentiality tracking into compiled artifacts are deferred.

This does not create a common Cargo `target/` directory, select the independent
[Nix Store Mode](store.md#selection-and-host-approval), authorize worker Celler
uploads, or broaden ordinary Celler read grants. Delegated compiler access to
its originating outputs is not cross-worker Cargo target reuse. Native tool
capacity controls remain separate; sharing is not a hard aggregate disk quota or
a new Wrix budget/eviction coordinator. Cargo/uv backing and shared-daemon
integration must meet these modes without turning ordinary directory staging
into a general write-back guarantee.

Caller-declared mounts retain the existing explicit mount contract; built-in
defaults do not make arbitrary consumer mounts safe. `sandbox.md` owns
launch/mount handling; this spec owns the built-in defaults, modes and tool
paths.

### Shared Rust Compiler Service

Acceptance: [Shared Compiler Acceptance](#shared-compiler-acceptance).

Shared Rust mode uses one actual sccache daemon in the existing host-user
service container, alongside Dolt, Celler and the proxy. It is not multiple
independent daemons writing one local cache directory, an HTTP blob-cache
replacement, or a separate compiler-only container/VM. Workers retain their
sccache client/wrapper; eligible cache misses can execute their selected
compiler in the service. Private mode instead uses worker-local compilation and
fresh private cache data, without registering inputs or connecting to the shared
compiler.

A restricted Rust launch must resolve to private mode: `run --no-cache`,
`SpawnConfig.tool_caches.shared = false`, or an inherited private repository
preference. A shared result combined with `WRIX_NETWORK=limit` is an actionable
configuration error before preparation; the launcher does not silently change
the resolved choice or weaken networking. No restricted launch receives shared
compiler delegation or a compiler-endpoint exception. Nix/Celler substitution
and scoped Dolt/proxy access are independent and remain available.

[Services](services.md#shared-compiler-service) owns daemon placement, capacity,
lifecycle and admitted source/toolchain/output views, with its policy verified
by [Compiler Service Acceptance](services.md#compiler-service-acceptance).
Concurrent requests must use the originating checkout, selected guest-platform
toolchain and output locations, including distinct Loom clones that share a
canonical repository identity. There is no common mutable Cargo `target/`
directory. A service-side cache hit must deliver the correct outputs to its
requesting worker, not merely return success or write another worker's files.

This deliberately moves compiler execution, including worker-supplied procedural
macros, into the service trust boundary. It does not provide
compiler/Dolt/Celler/ proxy compromise isolation. Compiler CPU/memory usage is
in the service runtime budget, not automatically charged to the requesting
worker's limits. [Security](security.md#shared-service-trust-boundary) owns
these residual risks. Personal-host cache/store/control access remains excluded;
full worker/runtime migration and per-job resource-assurance machinery are not
introduced.

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

`sandbox.devShell { ... }` is the recommended project entry point: it binds the
concrete sandbox's host profile and generated launch data to the host CLI under
[Project Launch Handoff](#project-launch-handoff). `wrix.mkDevShell` retains the
profile-only form, which does not invent a sandbox image/configuration:

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

Exactly one of `profile` or `sandbox` is accepted. A bound `sandbox.devShell`
does not accept either override. Composition order is:

| Field       | Rule                                                                            |
| ----------- | ------------------------------------------------------------------------------- |
| `packages`  | `profile.hostPackages ++ packages`                                              |
| `env`       | `profile.hostEnv // env` for ordinary keys; generated handoff keys are reserved |
| `shellHook` | internal lifecycle, then `profile.shellHook`, then consumer hook                |

Before Wrix-managed activation effects, the lifecycle admits the provided host
CLI under [Project Launch Handoff](#project-launch-handoff). It then runs
workspace services, project-cache integration, and hook-path selection before
the profile and consumer hooks. The cache policy belongs to `cache.md`. Host
activation publishes selected realized outputs without consuming the shared
cache or trusting its signing keys under
[Host Publication-only Integration](cache.md#host-publication-only-integration).
No host pull-token issuance/refresh or background renewer is introduced. Enabled
runtime activation also invokes
[Host Entry Publication](cache.md#host-activation-and-pre-push-publication) for
the selected already-realized environment, including first and cached direnv
activation. Cache opt-out suppresses this publication; no eager project warm is
introduced.

Acceptance: [Success Criteria](#success-criteria),
[Host Publication-only Acceptance](#host-publication-only-acceptance),
[Host Cache Publication Acceptance](#host-cache-publication-acceptance) and
[Project Handoff Acceptance](#project-handoff-acceptance).

#### Prek hook management

When `.pre-commit-config.yaml` exists and `prekHooks` resolves to a derivation,
the lifecycle first binds the absolute Nix-resolved runner for the current
platform/context, then sets local `core.hooksPath` to that derivation on every
shell entry. Reload repairs missing/stale bindings even when the hook path
already matches. `true` selects the default bundle, `false` performs no binding
or hook-path action, and a derivation substitutes the bundle. An existing
different path is overwritten with a one-line notice. Opting out leaves existing
state unchanged. `pre-commit.md` owns runtime resolution, compatibility, and
shared-worktree behavior.

## Project Launch Handoff

Acceptance: [Project Handoff Acceptance](#project-handoff-acceptance).

A consumer constructs one sandbox and uses `sandbox.devShell { }`; the
constructor automatically supplies that sandbox's generated immutable
`image.profileConfig` through a constructor-owned environment pointer. It keeps
profile, selected agent and image metadata together without consumer path/env
copying or independent pairing. Conflicting attempts to replace this generated
wiring through constructor environment overrides are evaluation errors. Explicit
per-invocation configuration remains available through the CLI, not a second
constructor-owned executable.

Neither devshell form adds a project Wrix CLI/`wrix-run` to host PATH, exports
project service-image overrides, or selects a project executable as a fallback.
Host CLI provision and explicit configuration selection follow
[Host CLI and Project Configuration](cli.md#host-cli-and-project-configuration).
Profile-only shells retain host tooling/lifecycle but do not supply an implicit
sandbox definition.

Actual activation, including first and cached/unchanged direnv activation,
requires a compatible host `wrix` on PATH before Wrix service, credential,
publication or hook-setup effects. Missing or unsupported host/config
combinations fail actionably; they do not provision a CLI, use another project
binary, fetch latest, or silently disable enabled integration. Nix realization
surrounding shell entry is distinct from these activation effects. Independent
host/project pins are not assumed compatible, and merely installing a newer host
package does not authorize a service update.

Recovery guidance offers Home Manager, nix-darwin, explicit
`nix profile add 'github:taheris/wrix#wrix'`, and explicit temporary provision
of the host package through Nix; no particular profile path is mandatory. These
are user actions, not automatic profile mutation. Launch parser/image guards
belong to [Sandbox Handoff](sandbox.md#project-configuration-handoff).

## Profile-Image Manifest

`wrix.lib.${system}.mkProfileImages` accepts profile names mapped to the
`.image` values returned by `mkSandbox`. It produces data-only JSON keyed first
by profile name and then by the image's selected agent. Each agent entry
contains `{ ref, source, source_kind, profile_config }`, with no executable
`launcher` field. Source and profile-config references retain Nix store context.
Image metadata is copied from the selected image; missing `profileConfig` is
rejected at evaluation.

Orchestrators invoke the provided host `wrix` with the selected
`--profile-config` and, for spawn, their per-launch `SpawnConfig`; they do not
receive or select another Rust CLI through this manifest. `image-builder.md`
owns source kind/composition and `sandbox.md` owns agent selection. Acceptance:
[Manifest and Output Acceptance](#manifest-and-output-acceptance).

Bundled `packages.profile-images` covers the built-in Pi images. External flakes
use the same data-only helper for custom profiles and explicit consumer direct
runners; there is no redundant `profile-images-pi` manifest.

## Flake Outputs

Acceptance: [Manifest and Output Acceptance](#manifest-and-output-acceptance).

The flake exposes the host `packages.wrix` and selects that host CLI as
`packages.default`. For built-in `base`, `rust` and `python` profiles,
`packages.image-<profile>[-<agent>]` exposes the selected OCI image source; the
unsuffixed image selects Pi, and explicit suffixes are `-claude` and `-pi`.
`packages.profile-images` supplies the Pi data manifest.

Runnable `packages.sandbox-*`, configured default/debug launcher packages, and
per-project `wrix`/`wrix-run` exports are absent, not forwarding or
compatibility aliases. Consumers use `sandbox.devShell { }` plus host
`wrix run`, or data outputs plus explicit host `spawn`. Direct consumers supply
their own agent package under `sandbox.md`; no placeholder direct runner is
manufactured. MCP selection remains orthogonal and is configured through
`mkSandbox`, not retained runnable MCP launcher variants.

## Repository Loom Tooling

The repository devshell supplies the native Loom CLI, while its packaged images
supply the Linux CLI from the same source revision. This applies across profile,
agent and MCP image variants; it does not add another Wrix executable to the
host devshell or preserve runnable default/debug sandbox packages. Reusable
profile-library constructors remain orchestrator-neutral.

Loom tracks `main` through a source-only input pinned by `flake.lock`. Building
the CLI uses the repository's existing package-builder inputs without evaluating
Loom's flake or following its Wrix input. Input updates refresh the source pin
without introducing a reciprocal flake dependency.

## Success Criteria

- The repository's Loom input tracks `main` as locked source only and does not
  carry transitive flake inputs [check](verify:profiles.loom-source-only)
- Repository outputs keep host-native and Linux-image Loom packages on their
  respective surfaces, from the same source revision, across all sandbox
  variants [check](verify:profiles.loom-package-wiring)
- The repository devshell exposes a working `loom gate verify --help` command
  without shipping the direct runner or mock agent as CLI tools
  [system](test-ci:test-loom-devshell)
- Every repository sandbox variant wires Loom into its assembled image
  environment; those environments execute `loom gate verify --help` on Linux
  [system](test-ci:test-loom-agent-images)
- Base profile provides functional development environment
  [judge](../tests/judges/profiles.sh#test_base_profile_functional)
- Base profile exposes `python3` on both image and host package surfaces for
  stdlib-only ad hoc scripting, while `uv`, `ruff`, `ty`, `UV_CACHE_DIR`, and
  managed uv backing remain Python-profile-only.
  [check](verify:profiles.base-python-boundary)
- Rust profile can compile and run a Cargo project with the toolchain and host
  package surface it exposes [check](verify:profiles.rust-compile-run)
- Rust profile toolchain survives nixpkgs updates (no dynamic linker breakage)
  [judge](../tests/judges/profiles.sh#test_rust_profile_rebuild_stable)
- rust-analyzer can resolve the standard library (RUST_SRC_PATH is set
  correctly) [judge](../tests/judges/profiles.sh#test_rust_analyzer_sysroot)
- `wrix.rustProfile { toolchain = ./rust-toolchain.toml; sha256 = "..."; }`
  produces a working profile whose `toolchain` field is a fenix-combine
  derivation reflecting the file's component set
  [judge](../tests/judges/profiles.sh#test_rust_profile_constructor)
- `wrix.rustProfile { toolchain; sha256; packages = [p]; hostPackages = [h]; env = { K = "v"; }; hostEnv = { H = "v"; }; runtimeSecrets = { TOKEN = "required"; }; mounts = [m]; networkAllowlist = [a]; }`
  lands extension args in the matching profile slots (package/mount/allowlist
  surfaces appended; environment and runtime-secret attrsets right-merged)
  [check](verify:profiles.rust-extension-args)
- `wrix.rustProfile {}` (omitting required `toolchain`/`sha256`) errors at
  evaluation rather than silently producing an unpinned profile
  [check](verify:profiles.rust-required-args)
- Sandbox-only Cargo registry/git paths and sccache data are writable so Cargo
  can fetch crates and sccache can cache artifacts without personal-host backing
  or `Read-only file system` errors.
  [system?](test-ci:test-profile-tool-cache-isolation-podman)
- A profile mount with `optional = true` is preserved in the Nix-generated
  `ProfileConfig` [check](test-ci:test-profile-config-wrapper)
- Both Linux and Darwin launch planners omit an optional profile mount when its
  expanded host source does not exist
  [test](command::launch::test::missing_optional_profile_mount_is_skipped_by_platform_planners)
- Python profile can run Python scripts with dependencies
  [judge](../tests/judges/profiles.sh#test_python_profile)
- Sandbox-only uv cache data is writable for cache misses without personal-host
  backing or `Read-only file system` errors.
  [system?](test-ci:test-profile-tool-cache-isolation-podman)
- deriveProfile correctly merges image packages, host packages, and environment
  [judge](../tests/judges/profiles.sh#test_derive_profile_merge)
- Built-in `corePackages` and `packages` equal the exact documented base, Rust,
  and Python package sets; a pinned Rust constructor substitutes its selected
  toolchain in `corePackages`; cargo-nextest is the only built-in Rust package
  outside `corePackages` [check](verify:profiles.core-membership)
- `deriveProfile p { packages = [extra]; }` appends `extra` to `.packages` but
  leaves `.corePackages` equal to `p.corePackages`, so `packages` −
  `corePackages` is exactly the downstream-added delta
  [check](verify:profiles.extra-packages-not-core)
- `deriveProfile p { packages = [image]; hostPackages = [host]; }` keeps image
  and host package extensions on their respective package surfaces without
  crossing either direction [check](verify:profiles.host-image-package-split)
- Profiles are composable (can extend extended profiles); `env` right-merges
  into image and host surfaces, `hostEnv` may override the host surface, and
  `runtimeSecrets` right-merges declarations while preserving built-in optional
  provider names [check](verify:profiles.nested-derive)
- `deriveProfile` and `rustProfile` accept valid runtime-secret declarations and
  reject malformed environment names or policies other than `"optional"` and
  `"required"` at profile construction
  [check](verify:profiles.runtime-secret-validation)
- `wrix.mkDevShell { profile = wrix.rustProfile { ... }; }` produces a devshell
  whose env contains an absolute `RUSTC` under `profile.toolchain`,
  `RUSTC_WRAPPER=sccache`, `SCCACHE_DIR`, `SCCACHE_CACHE_SIZE`, and no forced
  `CARGO_INCREMENTAL` value (the rust profile's `shellHook` was spliced)
  [check](verify:devshell.profile-shellhook-spliced)
- The rust devshell exports host-platform `RUSTC`, `RUST_SRC_PATH`,
  `LIBRARY_PATH`, and `OPENSSL_*` paths; on Darwin they differ from the Linux
  image paths in `profile.env` [check](verify:devshell.rust-host-env)
- `wrix.mkDevShell { profile; packages = [extra]; }` shell has both
  `profile.hostPackages` and `extra` available on PATH, while image-only
  `profile.packages` stay out of the host PATH
  [check](verify:devshell.host-packages-source)
- `wrix.mkDevShell { profile; env = { K = "v"; }; }` shell has env var `K=v`
  (right-merge with `profile.hostEnv`, consumer wins on conflict)
  [check](verify:devshell.env-right-merge)
- `wrix.mkDevShell { profile; shellHook = "marker_xyz"; }` shell hook contains
  both `profile.shellHook` content AND `marker_xyz`, with the consumer hook
  firing **after** the profile's [check](verify:devshell.shellhook-order)
- Devshell constructors reject missing or ambiguous profile selection:
  `wrix.mkDevShell {}` without `profile` or `sandbox`,
  `wrix.mkDevShell { sandbox = ...; profile = ...; }`, and
  `sandbox.devShell { profile = ...; }` / `sandbox.devShell { sandbox = ...; }`
  all error at evaluation. [check](verify:devshell.profile-required)
- `wrix.mkDevShell { profile = ...; }` places the `pre-push-checks` and
  `skip-if-missing` wrappers on the host devshell PATH
  [check](verify:prek.wrappers-on-devshell-path)
- `wrix.mkDevShell { profile = ...; }` with `.pre-commit-config.yaml` present
  sets `core.hooksPath` to the default hook bundle on entry
  [system](verify:devshell.prek-auto-set)
- Devshell entry repairs a missing/stale runner binding even when the canonical
  hook path already matches
  [test](../crates/wrix-cli/tests/prek_runtime.rs::devshell_entry_repairs_binding_even_when_hook_path_is_current)
- `wrix.mkDevShell { profile = ...; }` without `.pre-commit-config.yaml` does
  NOT set `core.hooksPath` on entry
  [system](verify:devshell.prek-skip-absent-config)
- `wrix.mkDevShell { profile = ...; prekHooks = false; }` does NOT set
  `core.hooksPath` even when `.pre-commit-config.yaml` is present
  [system](verify:devshell.prek-opt-out)
- `wrix.mkDevShell { profile = ...; prekHooks = <custom-derivation>; }` sets
  `core.hooksPath` to the substituted derivation when `.pre-commit-config.yaml`
  is present [system](verify:devshell.prek-derivation-substitute)
- When `prekHooks` resolves to a derivation and a previous session left
  `core.hooksPath` set to a different store path, entering `mkDevShell`
  overwrites it and prints a one-line message naming the old value (covers both
  the `true` default case and the substituted-derivation case)
  [system](verify:devshell.prek-stale-config-overwrite)
- `wrix.mkDevShell { profile = ...; prekHooks = false; }` entered in a repo
  whose local git config already has `core.hooksPath` set leaves that value
  unchanged (passive opt-out preserves stale state per design)
  [system](verify:devshell.prek-opt-out-preserves-stale-config)
- The mkDevShell implementation contains no `prek install` invocation and no
  `chmod` on `.git/hooks` [check](verify:devshell.no-prek-install)
- Flake-level devshell assembly does not set `core.hooksPath`; `mkDevShell` owns
  that lifecycle state
  [check](verify:devshell.flake-module-does-not-own-hooks-path)
- On Linux hosts, a host devshell built via
  `wrix.mkDevShell { profile = wrix.rustProfile { toolchain; sha256; }; }`
  resolves `rustc` to the same `/nix/store/...` path as the sandbox built from
  the same profile; on Darwin the host and image toolchains share the pinned
  channel/version but resolve to the platform-specific store paths described in
  Architecture
  [judge](../tests/judges/profiles.sh#test_host_sandbox_rustc_same_store_path)
- The public lib exposes toolchain identity through `profile.toolchain`, not
  `wrix.devToolchain` [check](verify:profiles.no-dev-toolchain-lib)
- Project-pinned profiles use top-level `wrix.rustProfile`;
  `profiles.rust.withToolchain` is not part of the profile attrset
  [check](verify:profiles.no-rust-with-toolchain)
- `profile.toolchain` is exposed on both `wrix.profiles.rust` and
  `wrix.rustProfile { toolchain; sha256; }`, and points at the same
  host-platform derivation `shellHook` interpolates into the PATH prepend
  (matches the image's toolchain in `profile.packages` on Linux hosts; uses the
  Darwin host counterpart on Darwin)
  [judge](../tests/judges/profiles.sh#test_rust_toolchain_field)
- `wrix.profiles.rust` and `wrix.rustProfile { toolchain; sha256; }` closures
  contain zero `*-nightly-*` derivations after a fresh `nix flake update`
  (regression guard against reintroducing
  `fenix.packages.${system}.rust-analyzer`, which drags a nightly
  cargo/rustc/rust-std closure) [check](verify:profiles.rust-no-nightly-closure)
- `mkProfileImages { rust = …; }` produces the selected-agent data shape with
  `ref`, `source`, `source_kind` and `profile_config`, no launcher executable,
  and Nix-context-preserving image/configuration references; missing
  configuration is an evaluation error.
  [check?](verify:profiles.data-only-manifest)
- Built-in image outputs select Pi by default and explicit Claude/Pi variants
  resolve to their selected sources, without a placeholder direct runner.
  [check?](verify:profiles.image-only-output-contract)
- The only Wrix CLI/default package is the host package; runnable sandbox/MCP
  variants, configured default/debug launchers and project `wrix-run` exports
  are absent, not aliases or host-CLI forwarding adapters.
  [check?](verify:profiles.host-cli-package-outputs)
- The bundled Pi manifest and custom direct manifests use the data-only contract
  with no per-entry executable or redundant Pi manifest.
  [check?](verify:profiles.pi-data-manifest)
- `profiles.rust.buildPackage` is exposed and returns an attrset with `bin`,
  `clippy`, `nextest`, and `cargoArtifacts` fields
  [check](verify:profiles.rust-build-package-exposed)
- Editing a workspace source file changes the `bin` derivation hash but does
  **not** change the `cargoArtifacts` derivation hash (dep cache reused across
  edits) [check](verify:profiles.rust-build-package-workspace-edit-reuses-deps)
- Source filter excludes non-Cargo files: editing a `README.md` or other `*.md`
  file inside `src` does **not** change the `bin`, `clippy`, or `nextest`
  derivation hashes
  [check](verify:profiles.rust-build-package-source-filter-excludes-noncargo)
- Editing a `.rs` file invalidates `bin`, `clippy`, and `nextest` together (the
  workspace source closure is shared by all three) but does **not** invalidate
  `cargoArtifacts`
  [check](verify:profiles.rust-build-package-workspace-edit-skips-cargo-artifacts)
- Repository integration-test edits invalidate the Rust check derivations but
  preserve the packaged launcher and Cargo dependency artifacts; production
  source edits still invalidate the launcher
  [check](verify:profiles.repository-integration-source-isolation)
- Editing a file in `extraSrcs` invalidates `clippy` and `nextest` but does
  **not** invalidate `bin` or `cargoArtifacts`
  [check](verify:profiles.rust-build-package-extra-srcs-scoped-to-checks)
- Cargo selects `${profile.toolchain}/bin/rustc` while building `bin`, `clippy`,
  and `nextest` for both `wrix.profiles.rust` and
  `wrix.rustProfile { toolchain; sha256; }`
  [check](verify:profiles.rust-build-package-toolchain-alignment)
- The repository devshell depends on the sandbox-owned `devShell` constructor
  rather than reconstructing profile toolchain or environment state
  [check](verify:devshell.sandbox-boundary)
- Platform container entrypoints contain no rustup bootstrap logic; the Rust
  toolchain is image-built
  [check](verify:profiles.sandbox-entrypoints-no-rustup)

### Project Handoff Acceptance

- The bound constructor automatically supplies its own generated launch
  configuration without a project CLI on host PATH or project service-image
  override; profile-only construction does not invent a sandbox definition.
  [check?](verify:devshell.host-cli-data-handoff)
- Constructor environment overrides cannot replace its generated launch wiring
  with independently paired configuration/image data.
  [check?](verify:devshell.generated-handoff-reserved)
- First and actual cached/unchanged activation use the provided host CLI and
  selected project's generated configuration without a profile-specific
  executable or manual configuration wiring.
  [system?](test-ci:test-devshell-host-cli-handoff)
- Missing/incompatible host CLI or unsupported project launch data blocks
  activation before Wrix service, credential, publication or hook-setup effects;
  no automatic install, second-binary fallback or silent integration opt-out.
  [system?](test-ci:test-devshell-host-cli-handoff)
- Explicitly providing the host package via a temporary Nix environment works
  without a persistent profile installation or fixed installation directory.
  [system?](test-ci:test-devshell-host-cli-handoff)
- Consumer setup guidance explains automatic constructor wiring and actionable
  Home Manager, nix-darwin, `nix profile add` and temporary-package recovery,
  without claiming independent pins are always compatible.
  [judge?](../tests/judges/global-wrix-consumer-api.md)

### Manifest and Output Acceptance

- The same host CLI accepts actual Nix-generated configurations from two
  projects via their data manifests and explicit `--profile-config`, without a
  manifest-provided launcher or legacy runnable target.
  [system?](test-ci:test-profile-config-global-cli)

### Tool Cache Isolation Acceptance

- Evaluated built-in profile defaults declare sandbox-only cache backing instead
  of personal-host cache mounts/prewarms, retain the guest tool paths and
  writable parents, and do not redirect host tool defaults into sandbox data.
  [check](verify:profiles.tool-cache-boundary)
- Production run/spawn cache preparation never binds/copies personal-host cache
  data or writes it back, including when managed data is missing; read-only
  preflight does not allocate or populate managed cache data.
  [test](../crates/wrix-sandbox/tests/tool_cache_isolation.rs::both_launch_modes_keep_tool_cache_preparation_host_separate)
- Live default Podman Rust/Python launches can populate writable sandbox caches
  while personal-host cache sentinels are neither visible nor modified.
  [system?](test-ci:test-profile-tool-cache-isolation-podman)
- Live krun Rust/Python launches demonstrate the same host-cache separation and
  writable tool behavior with their own required capability evidence.
  [system?](test-ci:test-profile-tool-cache-isolation-krun)
- Live Apple Rust/Python launches demonstrate the same host-cache separation and
  writable tool behavior, without personal-host prewarm copies and with their
  own required runtime/capability evidence.
  [system?](test-ci:test-profile-tool-cache-isolation-apple)

### Tool Cache Sharing Acceptance

- Both launch modes resolve explicit selection before selected-workspace repo
  policy before shared default, preserving omitted inheritance and both
  directions of explicit override, including re-enabling sharing over a private
  repo default.
  [test](../crates/wrix-sandbox/tests/tool_cache_isolation.rs::mode_precedence_follows_launch_repo_shared_default)
- One-off selections do not create/rewrite repository policy or persist as
  future defaults; later launches inherit their own current repository policy
  anew.
  [test](../crates/wrix-sandbox/tests/tool_cache_isolation.rs::launch_mode_overrides_do_not_write_repository_policy)

- Shared mode selects this host user's worker pool across repository/clone
  identities, not separate per-repository or personal-host cache backing.
  [test](../crates/wrix-sandbox/tests/tool_cache_isolation.rs::shared_mode_uses_host_user_pool_across_repositories)
- Sharing-off applies to every applicable managed cache in both launch modes,
  using fresh private writable data without shared-pool reads/writes/prewarms or
  retained per-repository cache allocation.
  [test](../crates/wrix-sandbox/tests/tool_cache_isolation.rs::private_mode_is_fresh_and_all_or_nothing)
- Mode changes neither promote private cache contents into the shared pool nor
  delete/recall previously shared data.
  [test](../crates/wrix-sandbox/tests/tool_cache_isolation.rs::mode_changes_do_not_migrate_or_delete_cache_data)
- Shared backing does not expose per-launch credential/configuration files or
  host/service management state as part of the cache pool.
  [test](../crates/wrix-sandbox/tests/tool_cache_isolation.rs::shared_backing_excludes_credentials_and_control_state)
- Live Podman workers from different repositories reuse compatible pooled tool
  data while concurrent use preserves correct tool results.
  [system?](test-ci:test-profile-tool-cache-isolation-podman)
- Live krun workers demonstrate cross-repository pooled reuse and correct
  concurrent tool results with their own required capability evidence.
  [system?](test-ci:test-profile-tool-cache-isolation-krun)
- Live Apple workers demonstrate cross-repository pooled reuse and correct
  concurrent tool results; copied startup data without retained updates is
  insufficient. [system?](test-ci:test-profile-tool-cache-isolation-apple)
- A sharing-off Podman launch can populate every applicable private cache while
  shared-pool sentinels are inaccessible and its private writes do not reach the
  pool. [system?](test-ci:test-profile-tool-cache-isolation-podman)
- A sharing-off krun launch demonstrates the same all-cache private data
  boundary with its own required capability evidence.
  [system?](test-ci:test-profile-tool-cache-isolation-krun)
- A sharing-off Apple launch demonstrates the same all-cache private data
  boundary, without shared-pool snapshots or personal-host fallback.
  [system?](test-ci:test-profile-tool-cache-isolation-apple)

### Shared Compiler Acceptance

- Shared Rust selects one service-owned compiler daemon; private mode keeps
  compilation/cache data local and never registers or connects to that daemon.
  [test?](../crates/wrix-sandbox/tests/tool_cache_isolation.rs::rust_modes_select_shared_service_or_private_local_compilation)
- Run/spawn reject restricted Rust with resolved shared mode before preparation,
  preserve normal precedence without silent coercion, and accept effective
  private selection without disabling Nix/Celler.
  [test](../crates/wrix-cli/tests/tool_cache_controls.rs::restricted_rust_requires_effective_private_tool_caches)
- Live Podman workers use the actual shared daemon for cache misses/hits and
  output delivery; concurrent distinct checkouts/toolchains do not mix results,
  private mode stays local, and ordinary compiler errors do not terminate
  services. [system?](test-ci:test-profile-sccache-service-podman)
- Live krun workers demonstrate the same real daemon reuse, request/output
  separation, private-mode behavior and compiler-error handling with honest KVM
  evidence. [system?](test-ci:test-profile-sccache-service-krun)
- Live Apple workers demonstrate the same real shared-daemon behavior across
  separate worker VMs; copied prewarms or merely sharing directory names do not
  pass. [system?](test-ci:test-profile-sccache-service-apple)

### Host Publication-only Acceptance

- Both devshell constructors, including real cached direnv activation, preserve
  host publication without shared-cache substitution/key trust or host
  pull-token issuance; inherited unsafe trust requires explicit remediation.
  [system?](test-ci:test-cache-host-publication-only)
- Cache opt-out performs no cache-only provisioning/publication or credential
  deletion and does not create a token-renewal worker.
  [system?](test-ci:test-cache-host-publication-only)

### Host Cache Publication Acceptance

- Both devshell constructors invoke automatic selected-environment publication
  on first and actual cached/unchanged activation without extra package/check
  builds. [system?](test-ci:test-cache-devshell-publication)
- Cache-disabled activation does not register/publish environment artifacts or
  provision cache-only services for this publication path.
  [system?](test-ci:test-cache-devshell-publication)

### Network Allowlist Acceptance

- Base, derived and project-pinned profile constructors accept structured
  `{ host, ports }` rules and reject bare strings, shorthand, unknown fields,
  empty hosts and missing/empty/non-integer/out-of-range ports.
  [check](verify:profiles.network-rule-contract)
- Built-in base/Rust/Python rules contain exactly the documented hosts and
  explicit ports, with no implicit web/SSH or all-port permissions.
  [check](verify:profiles.builtin-network-ports)
- Nested derivation and Rust constructor extensions preserve structured rules
  through list concatenation without adding unlisted host/port permissions.
  [check](verify:profiles.network-rule-extension)

## Requirements

### Functional

1. **Base Profile** — Image and host surfaces provide their documented core-tool
   sets, including `python3` for stdlib-only ad hoc agent scripting
2. **Language Profiles** — Pre-configured Rust and Python environments
3. **Profile Extension** — `deriveProfile` API to extend existing profiles
4. **Package Bundling** — Profiles specify `packages` to include in the
   container image and `hostPackages` to include in host devshells. A profile
   also exposes `corePackages`, the wrix-controlled fixed-per-instance subset of
   image packages, so the image builder can layer wrix-default content
   separately from downstream additions (see `image-builder.md` §
   Provenance-Tiered Layering).
5. **Environment Configuration** — Profiles separate non-secret image (`env`)
   and host (`hostEnv`) defaults from runtime-secret name/policy declarations;
   secret values are launcher inputs, not profile data
6. **Mount Specifications** — Profiles can define default mounts (e.g., cargo
   cache)
7. **Toolchain Configuration** — Top-level
   `rustProfile { toolchain; sha256; ... }` constructor produces a
   project-pinned rust profile from a `rust-toolchain.toml`
8. **Rust Package Construction** — Rust profile exposes `buildPackage` for
   crane-backed Rust packages with split `bin`/`clippy`/`nextest` derivations
9. **Devshell Construction** — `sandbox.devShell { ... }` is the preferred host
   devshell entry point when a concrete sandbox exists, and top-level
   `mkDevShell { profile; ... }` remains available for profile-only shells; both
   consume `profile.hostPackages` without supplying another Wrix CLI. Bound
   construction automatically delivers project configuration under
   [Project Launch Handoff](#project-launch-handoff); consumers do not splice
   `profile.shellHook` or manually wire configuration paths
10. **Prek Hook Management** — `mkDevShell` configures `core.hooksPath` from the
    hook derivation selected by `prekHooks` when `.pre-commit-config.yaml` is
    present, with `prekHooks = false` as the opt-out. The bundle's contents and
    shim behavior are owned by `specs/pre-commit.md`.

### Non-Functional

1. **Curated Toolkit** — Base profile is a ready-to-work agent toolkit, not a
   minimal OS layer.
2. **Reproducible** — Same profile produces the same tool/image configuration
   via Nix; compatibility with an independently managed host CLI is checked, not
   inferred from matching executable names.

## Out of Scope

- Per-project Wrix executables, configured launcher derivations/runnable sandbox
  aliases, and manifest-selected Rust binaries; image/configuration data is the
  project interface.
- Automatic installation/update of the host CLI or silent fallback when host
  setup is missing or incompatible.

- Language-specific project scaffolding
- IDE configuration beyond Claude Code
- Auto-detection of `rust-toolchain.toml` at runtime (must be passed explicitly
  via `rustProfile`)
- Selective directory, Git-repository or dependency exclusions; automatic
  private-source classification and tracking confidentiality into compiled
  artifacts. Use the coarse all-cache private mode instead.
- Automatic pruning of Cargo registry/git and uv sandbox caches, a hard
  aggregate disk quota or a custom budget coordinator. Native sccache capacity
  control remains `SCCACHE_CACHE_SIZE`, owned once by the service for shared
  mode and independently by local caches; it is not an all-tool filesystem
  quota. No personal-host cache maintenance or migration is authorized.
- Tracking nightly rust-analyzer in the default profile. Building
  `fenix.packages.${system}.rust-analyzer` from source pulls a matching nightly
  cargo/rustc/rust-std closure into every consumer's flake on each input update;
  the profile pins `fenix.stable.rust-analyzer-preview` instead. Consumers who
  need nightly RA opt in via `deriveProfile`.
- Cross-compilation in `buildPackage`. Always builds for
  `pkgs.stdenv.hostPlatform.system`. Consumers needing cross builds drop down to
  crane directly.
- Returning multiple `bin` outputs from a single `buildPackage` call. Workspaces
  with multiple binary crates call `buildPackage` once per binary, threading the
  same `cargoArtifacts` through to share dep compilation.
