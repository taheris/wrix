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

1. **Minimal Base** - Base profile includes only essential tools
2. **Reproducible** - Same profile produces same environment via Nix

## Built-in Profiles

### Base Profile

Core tools for any development environment:

| Package | Purpose |
|---------|---------|
| git | Version control |
| ripgrep | Fast text search |
| fd | Fast file finder |
| jq | JSON processing |
| vim | Text editor |
| openssh | SSH client |
| nix | Package manager |

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
| openssl | TLS library |
| pkg-config | Library discovery |
| postgresql.lib | Database client libs |

Environment:

- `CARGO_HOME=/workspace/.cargo` — persists registry cache on workspace volume
- `RUST_SRC_PATH=${toolchain}/lib/rustlib/src/rust/library` — rust-analyzer standard library resolution
- `LIBRARY_PATH=${pkgs.postgresql.lib}/lib` — PostgreSQL library discovery at link time
- `OPENSSL_INCLUDE_DIR=${pkgs.openssl.dev}/include` — OpenSSL headers
- `OPENSSL_LIB_DIR=${pkgs.openssl.out}/lib` — OpenSSL libraries
- `RUSTC_WRAPPER=${pkgs.sccache}/bin/sccache` — route compiler invocations through sccache
- `CARGO_BUILD_RUSTC_WRAPPER` — same, picked up by cargo directly
- `SCCACHE_DIR=/workspace/.cache/sccache` — stable in-container cache path, mounted from host
- `CARGO_TARGET_DIR=/workspace/.target-sandbox` — isolate sandbox incremental artifacts from the host's `target/`

Mounts:

- `~/.cargo/registry` (ro, optional) — pre-warm crate cache from host
- `~/.cargo/git` (ro, optional) — pre-warm git dependency cache from host
- `~/.cache/sccache` (rw, optional) — shared sccache store between host and sandbox

Network allowlist: `crates.io`, `static.crates.io`, `index.crates.io`

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

## Affected Files

| File | Role |
|------|------|
| `flake.nix` | Add `fenix` flake input, thread it through to `lib/sandbox/profiles.nix` |
| `lib/sandbox/profiles.nix` | Build rust toolchain via `fenixPkgs.stable.minimalToolchain` + `combine [ rust-src rust-analyzer ]`; wire `sccache`, `RUSTC_WRAPPER`, `SCCACHE_DIR`, `CARGO_TARGET_DIR` |
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

Downstream projects should gitignore both of the sandbox-only cache paths:

- `/workspace/.cache/sccache` — mounted from the host's `~/.cache/sccache` (rw, optional)
- `/workspace/.target-sandbox` — sandbox `CARGO_TARGET_DIR`, kept out of the host's `target/`
