{ pkgs, system, ... }:

let
  inherit (pkgs.lib) escapeShellArg;

  repoScript = path: function: ''
    run_repo_script ${escapeShellArg path} ${escapeShellArg function}
  '';

  wholeRepoScript = path: ''
    run_repo_script ${escapeShellArg path}
  '';
in
{
  "prek.bundle-contents" = ''
    ${repoScript "tests/profiles/prek-hooks-bundle.sh" "test_bundle_contents"}
    ${repoScript "tests/profiles/prek-hooks-bundle.sh" "test_bundle_path_is_context_stable"}
  '';
  "prek.shims-use-hook-impl" =
    repoScript "tests/profiles/prek-hooks-bundle.sh" "test_shims_use_hook_impl";
  "prek.shims-no-flock" = ''
    ${repoScript "tests/profiles/prek-hooks-bundle.sh" "test_shims_no_flock"}
    ${repoScript "tests/profiles/prek-hooks-bundle.sh" "test_shims_resolve_packaged_prek_at_runtime"}
  '';
  "prek.pre-push-stamp" =
    repoScript "tests/profiles/prek-hooks-bundle.sh" "test_pre_push_exact_transaction_stamp_written_and_consumed";
  "prek.pre-push-stamp-transaction-scope" =
    repoScript "tests/profiles/prek-hooks-bundle.sh" "test_pre_push_stamp_rejects_different_transaction";
  "prek.pre-push-stale-stamp" =
    repoScript "tests/profiles/prek-hooks-bundle.sh" "test_pre_push_stale_stamp_removed_on_failure";
  "prek.pre-push-stamp-cannot-revive" =
    repoScript "tests/profiles/prek-hooks-bundle.sh" "test_pre_push_stamp_cannot_revive_after_return_to_sha";
  "prek.no-verify-bypasses-hooks" =
    repoScript "tests/profiles/prek-hooks-bundle.sh" "test_no_verify_bypasses_pre_commit_and_pre_push";
  "prek.wrappers-on-devshell-path" =
    repoScript "tests/profiles/mkdevshell-prek.sh" "test_wrappers_exposed_and_on_devshell_path";
  "prek.config-stage-set" = repoScript "tests/prek/wrix-hook-stages.sh" "test_wrix_config_stage_set";
  "prek.formatter-cache-independent" = ''
    local root formatter
    root="$(repo_root)"
    formatter="$(build_flake_package formatter.${system})"
    python3 "$root/tests/prek/treefmt-cache.py" "$root" "$formatter/bin/treefmt"
  '';
  "prek.pre-push-checks-marker-valid" = wholeRepoScript "tests/prek/pre-push-checks-marker-valid.sh";
  "prek.pre-push-checks-marker-stale" = wholeRepoScript "tests/prek/pre-push-checks-marker-stale.sh";
  "prek.pre-push-checks-no-marker" = wholeRepoScript "tests/prek/pre-push-checks-no-marker.sh";
  "prek.pre-push-checks-no-metadata" = wholeRepoScript "tests/prek/pre-push-checks-no-metadata.sh";
  "prek.pre-push-checks-no-loom" = wholeRepoScript "tests/prek/pre-push-checks-no-loom.sh";
  "prek.skip-if-missing-present" = wholeRepoScript "tests/prek/skip-if-missing-present.sh";
  "prek.skip-if-missing-absent" = wholeRepoScript "tests/prek/skip-if-missing-absent.sh";
  "prek.wrapper-runtime-dependencies" = wholeRepoScript "tests/prek/wrapper-runtime-dependencies.sh";
  "prek.config-wrapper-contract" = wholeRepoScript "tests/prek/wrix-pre-push-config.sh";
  "prek.ci-only-heavy-checks" = wholeRepoScript "tests/prek/ci-only-heavy-checks.sh";
  "prek.ci-platform-policy" = wholeRepoScript "tests/prek/test-ci-platform-policy.sh";
  "prek.ci-batching" = ''
    local root test_ci_runner loom
    root="$(repo_root)"
    loom="$(build_flake_package loom)"
    nix run --no-warn-dirty "$root#test-ci" -- --list >/dev/null
    test_ci_runner=$(nix eval --raw --no-warn-dirty "$root#apps.${system}.test-ci.program")
    ${pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
      nix build --no-link --no-warn-dirty "$root#checks.${system}.system-test-prerequisites"
    ''}
    ${pkgs.python3}/bin/python3 "$root/tests/prek/test-ci-batching.py" \
      "$test_ci_runner" ${escapeShellArg system} ${pkgs.bash} ${pkgs.coreutils} ${../lib/verifier.sh} "$loom/bin/loom"
  '';
}
