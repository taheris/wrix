#!/usr/bin/env bash
set -euo pipefail

# Judge rubrics for profiles.md success criteria

test_base_profile_functional() {
  judge_files "lib/sandbox/profiles.nix"
  judge_criterion "Base profile provides a functional development environment with essential tools (git, curl, basic shell utilities, nftables as the primary firewall backend, and iptables as the fallback backend)"
}

test_rust_profile() {
  judge_files "lib/sandbox/profiles.nix"
  judge_criterion "Rust profile uses fenix (fenix.packages.\${system}.stable.defaultToolchain combined with fenix.stable.rust-src and fenix.stable.rust-analyzer-preview (manifest builds — NOT fenix.packages.\${system}.rust-analyzer which is built from nightly source)), gcc for linking, openssl, pkg-config, postgresql libs, and sccache. RUST_SRC_PATH, OPENSSL, RUSTC_WRAPPER, CARGO_BUILD_RUSTC_WRAPPER, SCCACHE_DIR, SCCACHE_CACHE_SIZE, and CARGO_INCREMENTAL=0 environment variables are configured. CARGO_HOME and CARGO_TARGET_DIR are not pinned (cargo's \$HOME/.cargo default applies; \$HOME=/home/wrix inside the container). No rustup, RUSTUP_HOME, or rust-overlay."
}

test_python_profile() {
  judge_files "lib/sandbox/profiles.nix"
  judge_criterion "Python profile includes Python interpreter and can run Python scripts with dependencies"
}

test_derive_profile_merge() {
  judge_files "lib/profile/default.nix" "lib/sandbox/profiles.nix"
  judge_criterion "deriveProfile correctly merges packages, hostPackages, mounts, env, and networkAllowlist from a base profile and extension attrset. packages/hostPackages/mounts/networkAllowlist are concatenated on their own surfaces; env is right-biased (extension wins). Other fields pass through from the extension if set, otherwise the base."
}

test_rust_profile_rebuild_stable() {
  judge_files "lib/sandbox/linux/entrypoint.sh" "lib/sandbox/darwin/entrypoint.sh"
  judge_criterion "Entrypoint scripts contain no rustup bootstrap logic (no rustup default, rustup component add, RUSTUP_HOME checks). Rust toolchain is provided entirely by fenix at image build time."
}

test_rust_analyzer_sysroot() {
  judge_files "lib/sandbox/profiles.nix"
  judge_criterion "RUST_SRC_PATH is set correctly so rust-analyzer can resolve the standard library sysroot."
}

test_rust_profile_constructor() {
  judge_files "lib/default.nix" "lib/profile/default.nix" "lib/sandbox/profiles.nix" "lib/sandbox/default.nix"
  judge_criterion "wrix.rustProfile is a top-level constructor in lib/default.nix (alongside deriveProfile/mkDevShell/mkSandbox) with signature { toolchain, sha256, packages ? [], hostPackages ? [], env ? {}, mounts ? [], networkAllowlist ? [] }. Both toolchain and sha256 are required (Nix destructuring errors when omitted; no silent unpinned-profile fallback). Internally it consumes fenix.fromToolchainFile via the helper exposed from lib/sandbox/profiles.nix, combines rust-src and fenix.stable.rust-analyzer-preview on top with fenix.combine, then applies extension args using the same merge rules as deriveProfile (packages/hostPackages/mounts/networkAllowlist concatenated on their own surfaces, env right-biased — consumer wins on conflict). Pass-through fields (name/enabledPlugins/shellHook/writableDirs/toolchain/buildPackage) come from the pinned-profile base. profiles.rust.withToolchain is no longer exposed."
}

test_host_sandbox_rustc_same_store_path() {
  judge_files "lib/sandbox/profiles.nix" "lib/default.nix"
  judge_criterion "The rust profile exposes a shellHook field (not hostShellHook) that, when the profile is consumed via wrix.mkDevShell { profile = ...; } (the consumer never splices profile.shellHook directly), prepends the host-platform toolchain's bin directory to PATH. On Linux hosts that host toolchain is the same /nix/store/... derivation the sandbox image bakes in, so host and sandbox rustc paths match. On Darwin hosts the host and image toolchains share the pinned channel/version but intentionally resolve to platform-specific store paths per specs/profiles.md § Host/image toolchain split."
}

test_cargo_registry_writable() {
  judge_files "lib/sandbox/profiles.nix"
  judge_criterion "The rust profile's mounts for ~/.cargo/registry → /home/wrix/.cargo/registry, ~/.cargo/git → /home/wrix/.cargo/git, and ~/.cache/sccache → /home/wrix/.cache/sccache are mode = \"rw\" (not \"ro\"), so cargo can write new crates and git checkouts while sccache writes compiler artifacts on cache miss without 'Read-only file system (os error 30)' errors. writableDirs includes both /home/wrix/.cargo and /home/wrix/.cache so the launcher stacks Linux tmpfs mounts there with U=true; /home/wrix/.cargo covers cargo sibling files like .global-cache/credentials.toml, and /home/wrix/.cache is an intentional sccache parent that stays writable when the optional ~/.cache/sccache host mount is absent. The Darwin entrypoint is exempt — its rw cache mounts are session-scoped (writes stay in the writable layer and are discarded at exit), and writableDirs is Linux-only by design."
}

test_uv_cache_writable() {
  judge_files "lib/sandbox/profiles.nix"
  judge_criterion "The python profile's mount for ~/.cache/uv → /home/wrix/.cache/uv is mode = \"rw\" (not \"ro\"), the dest is the literal path /home/wrix/.cache/uv (no \"~\" since the launcher expands tildes on the host), and env.UV_CACHE_DIR equals /home/wrix/.cache/uv so uv reads from and writes back to the same mount destination. This mirrors the cargo registry writable invariant: ro here would break any uv invocation that needs a package not in the pre-warm set with 'Read-only file system (os error 30)' on the cache write path."
}

test_rust_toolchain_field() {
  judge_files "lib/sandbox/profiles.nix" "lib/default.nix"
  judge_criterion "The rust profile exposes a 'toolchain' field on both the default profile and the wrix.rustProfile { toolchain; sha256; } constructor. The field is the host-platform resolved fenix combine derivation and is the same derivation interpolated into the shellHook PATH prepend and used by buildPackage/crane. On Linux hosts this also matches the image toolchain in profile.packages; on Darwin hosts profile.packages/RUST_SRC_PATH keep the Linux image toolchain while profile.toolchain remains host-platform, so sibling Nix apps using rustProfile.toolchain in runtimeInputs align with the host devshell rather than the image store path."
}
