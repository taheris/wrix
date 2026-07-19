{ pkgs, system }:

let
  inherit (pkgs.lib) escapeShellArg;

  serviceScript = script: function: ''
    run_repo_script ${escapeShellArg "tests/services/${script}.sh"} ${escapeShellArg function}
  '';
  linuxSystem =
    if pkgs.stdenv.isDarwin then builtins.replaceStrings [ "darwin" ] [ "linux" ] system else system;
  beadsSystem = ''
    local root
    root="$(repo_root)"
    nix build --no-link --no-warn-dirty \
      "$root#legacyPackages.${linuxSystem}.systemTests.beads-live-system"
  '';
in
{
  "beads.dolt-sync-in-container" = beadsSystem;

  "beads.dolt-sync-uses-container-remote" = beadsSystem;

  "beads.no-jsonl-staged" = serviceScript "dolt-cli" "test_no_jsonl_staged";

  "beads.shellhook-fail-loud" = serviceScript "beads-shellhook" "test_shellhook_fail_loud";

  "beads.shellhook-lifecycle-isolation" = beadsSystem;

  "beads.workspace-naming-determinism" =
    serviceScript "dolt-endpoints" "test_workspace_naming_determinism";
}
