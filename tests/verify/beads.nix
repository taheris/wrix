{ pkgs, ... }:

let
  inherit (pkgs.lib) escapeShellArg;

  serviceScript = script: function: ''
    run_repo_script ${escapeShellArg "tests/services/${script}.sh"} ${escapeShellArg function}
  '';
in
{
  "beads.dolt-sync-in-container" = serviceScript "dolt-cli" "test_sync_in_container";

  "beads.dolt-sync-uses-container-remote" =
    serviceScript "dolt-cli" "test_bd_dolt_sync_uses_container_remote";

  "beads.no-jsonl-staged" = serviceScript "dolt-cli" "test_no_jsonl_staged";

  "beads.shellhook-fail-loud" = serviceScript "beads-shellhook" "test_shellhook_fail_loud";

  "beads.shellhook-lifecycle-isolation" =
    serviceScript "beads-shellhook" "test_shellhook_lifecycle_isolation";

  "beads.workspace-naming-determinism" =
    serviceScript "dolt-endpoints" "test_workspace_naming_determinism";
}
