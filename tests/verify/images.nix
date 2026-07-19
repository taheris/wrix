{ pkgs, ... }:

let
  inherit (pkgs.lib) escapeShellArg;

  entrypoint = function: ''
    run_repo_script ${escapeShellArg "tests/sandbox/entrypoint-contract.sh"} ${escapeShellArg function}
  '';
in
{
  "images.darwin-entrypoint-core-hooks-path" = entrypoint "test_darwin_core_hooks_path";
  "images.linux-entrypoint-core-hooks-path" = entrypoint "test_linux_core_hooks_path";
}
