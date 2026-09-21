{ pkgs, ... }:

let
  inherit (pkgs.lib) escapeShellArg;

  serviceScript = script: function: ''
    run_repo_script ${escapeShellArg "tests/services/${script}.sh"} ${escapeShellArg function}
  '';
  sandboxScript = script: function: ''
    run_repo_script ${escapeShellArg "tests/sandbox/${script}.sh"} ${escapeShellArg function}
  '';
in
{
  "beads.no-embedded-fallback" =
    serviceScript "dolt-cli" "test_entrypoints_reject_embedded_dolt_and_jsonl_fallback";

  "beads.darwin-remote-remap" = sandboxScript "entrypoint-contract" "test_darwin_bd_remote_remap";

  "beads.sandbox-readiness" =
    sandboxScript "entrypoint-contract" "test_stale_beads_endpoint_blocks_agent_both";

  "beads.shared-access" = ''
    local beads
    beads="$(build_flake_package beads)"
    WRIX_TEST_BD_BIN="$beads/bin/bd" PATH="${pkgs.dolt}/bin:$PATH" run_repo_script tests/services/beads-access.sh
  '';

  "beads.shellhook-darwin-runtime-fallback" =
    serviceScript "beads-shellhook" "test_darwin_shellhook_selects_podman_fallback";

  "beads.shellhook-endpoint-fail-loud" =
    serviceScript "beads-shellhook" "test_shellhook_unreachable_endpoint_fails_loud";

  "beads.shellhook-runtime-fail-loud" =
    serviceScript "beads-shellhook" "test_shellhook_missing_runtime_fails_loud";

  "beads.tracked-files" = ''
    local actual
    local expected
    local root
    root="$(repo_root)"
    expected="$(printf '%s\n' '.beads/.gitignore' '.beads/config.yaml' '.beads/metadata.json')"
    actual="$(git -C "$root" ls-files -- .beads)"

    if [[ "$actual" != "$expected" ]]; then
      printf 'Expected tracked .beads files:\n%s\n' "$expected" >&2
      printf 'Actual tracked .beads files:\n%s\n' "$actual" >&2
      return 1
    fi
  '';
}
