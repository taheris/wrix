#!/usr/bin/env bash
# Judge rubrics for profiles.md success criteria

test_base_profile_functional() {
  judge_files "lib/sandbox/profiles.nix"
  judge_criterion "Base profile provides a functional development environment with essential tools (git, curl, basic shell utilities)"
}

test_rust_profile() {
  judge_files "lib/sandbox/profiles.nix"
  judge_criterion "Rust profile uses fenix (fenix.packages.\${system}.stable.defaultToolchain combined with fenix rust-src and rust-analyzer), gcc for linking, openssl, pkg-config, postgresql libs, and sccache. CARGO_HOME, RUST_SRC_PATH, OPENSSL, RUSTC_WRAPPER, CARGO_BUILD_RUSTC_WRAPPER, SCCACHE_DIR, SCCACHE_CACHE_SIZE, and CARGO_INCREMENTAL=0 environment variables are configured. CARGO_TARGET_DIR is not pinned (cargo default applies). No rustup, RUSTUP_HOME, or rust-overlay."
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

test_rust_with_toolchain() {
  judge_files "lib/sandbox/profiles.nix"
  judge_criterion "profiles.rust.withToolchain accepts { file, sha256 } (file being a rust-toolchain.toml path, sha256 being the component hash fenix needs for purity) and returns a profile attrset (without withToolchain) using fenix.fromToolchainFile. rust-src and rust-analyzer are combined in via fenix.combine. The returned profile is compatible with deriveProfile."
}

test_host_sandbox_rustc_same_store_path() {
  judge_files "lib/sandbox/profiles.nix"
  judge_criterion "The rust profile exposes a shellHook field (not hostShellHook) that, when spliced into a host devShell, prepends \${toolchain}/bin to PATH so host rustc resolves to the same /nix/store/... path the sandbox bakes in. Both the default rust profile and the withToolchain variant must close their shellHook over the same toolchain derivation that goes into packages, so packages and shellHook never drift."
}

test_cargo_registry_writable() {
  judge_files "lib/sandbox/profiles.nix"
  judge_criterion "The rust profile's mounts for ~/.cargo/registry → /home/wrapix/.cargo/registry and ~/.cargo/git → /home/wrapix/.cargo/git are mode = \"rw\" (not \"ro\"), so cargo can write new crates and git checkouts on cache miss without 'Read-only file system (os error 30)' errors. writableDirs includes /home/wrapix/.cargo so the launcher stacks a tmpfs there with U=true; without this, podman creates the bind-mount parent as root and cargo cannot write sibling files like .global-cache/credentials.toml even though the registry/git binds themselves are rw. The Darwin entrypoint is exempt — its rw cache mounts are session-scoped (writes stay in the writable layer and are discarded at exit), and writableDirs is Linux-only by design."
}

test_rust_toolchain_field() {
  judge_files "lib/sandbox/profiles.nix"
  judge_criterion "The rust profile exposes a 'toolchain' field on both the default profile and the withToolchain { file, sha256 } variant. The field is the resolved fenix combine derivation — the same derivation interpolated into the profile's packages list and into the \${toolchain}/bin PATH prepend in shellHook. All three references close over the same value so sibling Nix apps using rustProfile.toolchain in runtimeInputs see the identical /nix/store/... path the sandbox bakes in."
}
