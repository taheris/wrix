{ pkgs, system, ... }:

let
  inherit (pkgs.lib) escapeShellArg;

  repoScript = path: function: ''
    run_repo_script ${escapeShellArg path} ${escapeShellArg function}
  '';

  serviceScriptWithWrix = script: function: ''
    run_repo_script_with_wrix ${escapeShellArg "tests/services/${script}.sh"} ${escapeShellArg function}
  '';

  nixEval = target: ''
    local root
    root="$(repo_root)"
    REPO_ROOT="$root" VERIFY_SYSTEM=${escapeShellArg system} VERIFY_TARGET=${escapeShellArg target} nix eval --raw --impure --no-warn-dirty --expr '
      import (builtins.getEnv "REPO_ROOT" + "/tests/verify/profiles-eval.nix") {
        root = builtins.getEnv "REPO_ROOT";
        system = builtins.getEnv "VERIFY_SYSTEM";
        target = builtins.getEnv "VERIFY_TARGET";
      }
    ' >/dev/null
  '';

  buildPackage = function: repoScript "tests/profiles/build-package.sh" function;
  corePackages = function: repoScript "tests/profiles/core-packages.sh" function;
  mkDevShell = function: repoScript "tests/profiles/mkdevshell.sh" function;
  mkDevShellPrek = function: repoScript "tests/profiles/mkdevshell-prek.sh" function;
  profileComposition = function: repoScript "tests/profiles/profile-composition.sh" function;
  profileImages = function: repoScript "tests/profiles/profile-images-manifest.sh" function;
  rustProfileCtor = function: repoScript "tests/profiles/rust-profile-ctor.sh" function;
in
{
  "devshell.env-right-merge" = mkDevShell "test_env_right_merge";
  "devshell.flake-module-does-not-own-hooks-path" =
    nixEval "devshell.flake-module-does-not-own-hooks-path";
  "devshell.flake-module-thin-consumer" = nixEval "devshell.flake-module-thin-consumer";
  "devshell.host-packages-source" = mkDevShell "test_host_packages_source";
  "devshell.nix-cache" = serviceScriptWithWrix "host-nix-config" "test_mkdevshell_nix_cache";
  "devshell.no-prek-install" = nixEval "devshell.no-prek-install";
  "devshell.prek-auto-set" = mkDevShellPrek "test_auto_set_when_config_present";
  "devshell.prek-derivation-substitute" = mkDevShellPrek "test_derivation_substitute";
  "devshell.prek-opt-out" = mkDevShellPrek "test_opt_out";
  "devshell.prek-opt-out-preserves-stale-config" =
    mkDevShellPrek "test_opt_out_preserves_stale_config";
  "devshell.prek-skip-absent-config" = mkDevShellPrek "test_skip_when_config_absent";
  "devshell.prek-stale-config-overwrite" = mkDevShellPrek "test_stale_config_overwrite_with_warning";
  "devshell.profile-required" = mkDevShell "test_profile_required";
  "devshell.profile-shellhook-spliced" = mkDevShell "test_profile_shellhook_spliced";
  "devshell.shellhook-order" = nixEval "devshell.shellhook-order";

  "profiles.base-python-boundary" = corePackages "test_base_python_boundary";
  "profiles.core-membership" = corePackages "test_core_membership";
  "profiles.extra-packages-not-core" = corePackages "test_extra_not_in_core";
  "profiles.host-image-package-split" = profileComposition "test_host_packages_split";
  "profiles.image-flake-outputs" = profileImages "test_flake_outputs_present";
  "profiles.images-manifest-shape" = profileImages "test_manifest_shape";
  "profiles.nested-derive" = profileComposition "test_nested_derive_profile";
  "profiles.no-dev-toolchain-lib" = nixEval "profiles.no-dev-toolchain-lib";
  "profiles.no-rust-with-toolchain" = nixEval "profiles.no-rust-with-toolchain";
  "profiles.rust-build-package-consumers-migrated" = buildPackage "test_consumers_migrated";
  "profiles.rust-build-package-exposed" = buildPackage "test_build_package_exposed";
  "profiles.rust-build-package-extra-srcs-scoped-to-checks" =
    buildPackage "test_extra_srcs_scoped_to_lint_test";
  "profiles.rust-build-package-source-filter-excludes-noncargo" =
    buildPackage "test_source_filter_excludes_non_cargo";
  "profiles.rust-build-package-toolchain-alignment" =
    buildPackage "test_build_package_toolchain_alignment";
  "profiles.rust-build-package-workspace-edit-reuses-deps" =
    buildPackage "test_workspace_edit_reuses_dep_cache";
  "profiles.rust-build-package-workspace-edit-skips-cargo-artifacts" =
    buildPackage "test_workspace_edit_skips_cargo_artifacts";
  "profiles.rust-extension-args" = rustProfileCtor "test_extension_args";
  "profiles.rust-no-nightly-closure" =
    repoScript "tests/profiles/no-nightly-closure.sh" "test_no_nightly_closure";
  "profiles.rust-required-args" = rustProfileCtor "test_required_args";
  "profiles.sandbox-entrypoints-no-rustup" = nixEval "profiles.sandbox-entrypoints-no-rustup";
}
