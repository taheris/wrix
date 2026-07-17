{ pkgs, ... }:

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
  "prek.bundle-contents" = repoScript "tests/profiles/prek-hooks-bundle.sh" "test_bundle_contents";
  "prek.devshell-auto-set" =
    repoScript "tests/profiles/mkdevshell-prek.sh" "test_auto_set_when_config_present";
  "prek.shims-use-hook-impl" =
    repoScript "tests/profiles/prek-hooks-bundle.sh" "test_shims_use_hook_impl";
  "prek.shims-no-flock" = repoScript "tests/profiles/prek-hooks-bundle.sh" "test_shims_no_flock";
  "prek.pre-push-stamp" =
    repoScript "tests/profiles/prek-hooks-bundle.sh" "test_pre_push_stamp_written_and_consumed";
  "prek.wrappers-on-devshell-path" =
    repoScript "tests/profiles/mkdevshell-prek.sh" "test_wrappers_exposed_and_on_devshell_path";
  "prek.config-stage-set" = repoScript "tests/prek/wrix-hook-stages.sh" "test_wrix_config_stage_set";
  "prek.pre-push-checks-marker-valid" = wholeRepoScript "tests/prek/pre-push-checks-marker-valid.sh";
  "prek.pre-push-checks-marker-stale" = wholeRepoScript "tests/prek/pre-push-checks-marker-stale.sh";
  "prek.pre-push-checks-no-marker" = wholeRepoScript "tests/prek/pre-push-checks-no-marker.sh";
  "prek.pre-push-checks-no-metadata" = wholeRepoScript "tests/prek/pre-push-checks-no-metadata.sh";
  "prek.pre-push-checks-no-loom" = wholeRepoScript "tests/prek/pre-push-checks-no-loom.sh";
  "prek.skip-if-missing-present" = wholeRepoScript "tests/prek/skip-if-missing-present.sh";
  "prek.skip-if-missing-absent" = wholeRepoScript "tests/prek/skip-if-missing-absent.sh";
  "prek.config-wrapper-contract" = wholeRepoScript "tests/prek/wrix-pre-push-config.sh";
  "prek.container-pre-commit" = wholeRepoScript "tests/sandbox/container-pre-commit.sh";
  "prek.container-pre-push" = wholeRepoScript "tests/sandbox/container-pre-push.sh";
  "prek.ci-only-heavy-checks" = wholeRepoScript "tests/prek/ci-only-heavy-checks.sh";
}
