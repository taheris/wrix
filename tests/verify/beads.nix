{ pkgs, ... }:

let
  inherit (pkgs.lib) escapeShellArg;

  serviceScript = script: function: ''
    run_repo_script ${escapeShellArg "tests/services/${script}.sh"} ${escapeShellArg function}
  '';
in
{
  "beads.no-jsonl-staged" = serviceScript "dolt-cli" "test_no_jsonl_staged";

  "beads.shellhook-fail-loud" = serviceScript "beads-shellhook" "test_shellhook_fail_loud";

  "beads.workspace-naming-determinism" =
    serviceScript "dolt-endpoints" "test_workspace_naming_determinism";
}
