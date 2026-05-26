#!/usr/bin/env bash
# Judge rubrics for profiles.md success criteria

test_base_profile_functional() {
  judge_files "lib/sandbox/profiles.nix"
  judge_criterion "Base profile provides a functional development environment with essential tools (git, curl, basic shell utilities)"
}

test_rust_profile() {
  judge_files "lib/sandbox/profiles.nix"
  judge_criterion "Rust profile uses fenix (fenix.packages.\${system}.stable.defaultToolchain combined with fenix.stable.rust-src and fenix.stable.rust-analyzer-preview (manifest builds — NOT fenix.packages.\${system}.rust-analyzer which is built from nightly source)), gcc for linking, openssl, pkg-config, postgresql libs, and sccache. CARGO_HOME, RUST_SRC_PATH, OPENSSL, RUSTC_WRAPPER, CARGO_BUILD_RUSTC_WRAPPER, SCCACHE_DIR, SCCACHE_CACHE_SIZE, and CARGO_INCREMENTAL=0 environment variables are configured. CARGO_TARGET_DIR is not pinned (cargo default applies). No rustup, RUSTUP_HOME, or rust-overlay."
}

test_python_profile() {
  judge_files "lib/sandbox/profiles.nix"
  judge_criterion "Python profile includes Python interpreter and can run Python scripts with dependencies"
}

test_derive_profile_merge() {
  judge_files "lib/default.nix" "lib/sandbox/profiles.nix"
  judge_criterion "deriveProfile (in lib/default.nix) correctly merges packages, mounts, env, and networkAllowlist from a base profile and extension attrset. packages/mounts/networkAllowlist are concatenated; env is right-biased (extension wins). Other fields pass through from the extension if set, otherwise the base."
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
  judge_files "lib/default.nix" "lib/sandbox/profiles.nix" "lib/sandbox/default.nix"
  judge_criterion "wrapix.rustProfile is a top-level constructor in lib/default.nix (alongside deriveProfile/mkDevShell/mkSandbox) with signature { toolchain, sha256, packages ? [], env ? {}, mounts ? [], networkAllowlist ? [] }. Both toolchain and sha256 are required (Nix destructuring errors when omitted; no silent unpinned-profile fallback). Internally it consumes fenix.fromToolchainFile via the helper exposed from lib/sandbox/profiles.nix, combines rust-src and fenix.stable.rust-analyzer-preview on top with fenix.combine, then applies extension args using the same merge rules as deriveProfile (packages/mounts/networkAllowlist concatenated, env right-biased — consumer wins on conflict). Pass-through fields (name/enabledPlugins/shellHook/writableDirs/toolchain/buildPackage) come from the pinned-profile base. profiles.rust.withToolchain is no longer exposed."
}

test_host_sandbox_rustc_same_store_path() {
  judge_files "lib/sandbox/profiles.nix" "lib/default.nix"
  judge_criterion "The rust profile exposes a shellHook field (not hostShellHook) that, when the profile is consumed via wrapix.mkDevShell { profile = ...; } (the consumer never splices profile.shellHook directly), prepends \${toolchain}/bin to PATH so host rustc resolves to the same /nix/store/... path the sandbox bakes in. Both the default rust profile and the wrapix.rustProfile { toolchain; sha256; } constructor must close profile.packages and the \${toolchain}/bin prepend in shellHook over the same toolchain derivation, so packages and shellHook never drift."
}

test_cargo_registry_writable() {
  judge_files "lib/sandbox/profiles.nix"
  judge_criterion "The rust profile's mounts for ~/.cargo/registry → /home/wrapix/.cargo/registry and ~/.cargo/git → /home/wrapix/.cargo/git are mode = \"rw\" (not \"ro\"), so cargo can write new crates and git checkouts on cache miss without 'Read-only file system (os error 30)' errors. writableDirs includes /home/wrapix/.cargo so the launcher stacks a tmpfs there with U=true; without this, podman creates the bind-mount parent as root and cargo cannot write sibling files like .global-cache/credentials.toml even though the registry/git binds themselves are rw. The Darwin entrypoint is exempt — its rw cache mounts are session-scoped (writes stay in the writable layer and are discarded at exit), and writableDirs is Linux-only by design."
}

test_uv_cache_writable() {
  judge_files "lib/sandbox/profiles.nix"
  judge_criterion "The python profile's mount for ~/.cache/uv → /home/wrapix/.cache/uv is mode = \"rw\" (not \"ro\"), the dest is the literal path /home/wrapix/.cache/uv (no \"~\" since the launcher expands tildes on the host), and env.UV_CACHE_DIR equals /home/wrapix/.cache/uv so uv reads from and writes back to the same mount destination. This mirrors the cargo registry writable invariant: ro here would break any uv invocation that needs a package not in the pre-warm set with 'Read-only file system (os error 30)' on the cache write path."
}

test_rust_toolchain_field() {
  judge_files "lib/sandbox/profiles.nix" "lib/default.nix"
  judge_criterion "The rust profile exposes a 'toolchain' field on both the default profile and the wrapix.rustProfile { toolchain; sha256; } constructor. The field is the resolved fenix combine derivation — the same derivation interpolated into the profile's packages list and into the \${toolchain}/bin PATH prepend in shellHook. All three references close over the same value so sibling Nix apps using rustProfile.toolchain in runtimeInputs see the identical /nix/store/... path the sandbox bakes in."
}
