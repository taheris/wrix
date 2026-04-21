#!/usr/bin/env bash
# Judge rubrics for profiles.md success criteria

test_base_profile_functional() {
  judge_files "lib/sandbox/profiles.nix"
  judge_criterion "Base profile provides a functional development environment with essential tools (git, curl, basic shell utilities)"
}

test_rust_profile() {
  judge_files "lib/sandbox/profiles.nix"
  judge_criterion "Rust profile uses fenix (fenix.packages.\${system}.stable.defaultToolchain combined with fenix rust-src and rust-analyzer), gcc for linking, openssl, pkg-config, postgresql libs, and sccache. CARGO_HOME, RUST_SRC_PATH, OPENSSL, RUSTC_WRAPPER, CARGO_BUILD_RUSTC_WRAPPER, SCCACHE_DIR, and CARGO_TARGET_DIR environment variables are configured. No rustup, RUSTUP_HOME, or rust-overlay."
}

test_python_profile() {
  judge_files "lib/sandbox/profiles.nix"
  judge_criterion "Python profile includes Python interpreter and can run Python scripts with dependencies"
}

test_derive_profile_merge() {
  judge_files "lib/sandbox/profiles.nix"
  judge_criterion "deriveProfile correctly merges packages and environment variables from base and extension profiles"
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
