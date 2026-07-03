{ pkgs, ... }:

let
  inherit (pkgs.lib) escapeShellArg;

  tmuxScript = script: ''
    run_repo_script ${escapeShellArg "tests/mcp/tmux/${script}.sh"}
  '';
in
{
  "tmux-mcp.e2e-sandbox" = tmuxScript "e2e-sandbox";

  "tmux-mcp.integration" = tmuxScript "integration";
}
