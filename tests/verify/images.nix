{ pkgs, system, ... }:

let
  inherit (pkgs.lib) escapeShellArg;

  entrypoint = function: ''
    run_repo_script ${escapeShellArg "tests/sandbox/entrypoint-contract.sh"} ${escapeShellArg function}
  '';
in
{
  "images.inspection-artifacts" = {
    inputs = (import ../lib/image-resources.nix { }).imageInputs;
    script = ''
      nix build --no-link ".#checks.${system}.image-verifier-artifacts"
    '';
  };
  "images.darwin-entrypoint-core-hooks-path".script = entrypoint "test_darwin_core_hooks_path";
  "images.linux-entrypoint-core-hooks-path".script = entrypoint "test_linux_core_hooks_path";
  "images.linked-worktree-core-hooks-path".script =
    entrypoint "test_linked_worktree_core_hooks_path_both";
  "images.missing-hook-runtime-blocks-agent".script =
    entrypoint "test_missing_hook_runtime_blocks_agent_both";
}
