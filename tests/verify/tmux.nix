{ pkgs, system }:
let
  inherit (pkgs.lib) escapeShellArg;
in
{
  "tmux.native-only-surface" = ''
    local root
    root="$(repo_root)"
    REPO_ROOT="$root" VERIFY_SYSTEM=${escapeShellArg system} nix eval --raw --impure --no-warn-dirty --expr '
      import (builtins.getEnv "REPO_ROOT" + "/tests/verify/tmux-eval.nix") {
        root = builtins.getEnv "REPO_ROOT";
        system = builtins.getEnv "VERIFY_SYSTEM";
      }
    '
    nix build --no-link --no-warn-dirty "$root#checks.${system}.tmux-guidance-commands" "$root#checks.${system}.tmux-workflow-syntax"
  '';
  "tmux.retired-selection-rejected" = ''
    local root
    root="$(repo_root)"
    nix build --no-link --no-warn-dirty "$root#checks.${system}.tmux-retired-selection-rejected"
  '';
}
