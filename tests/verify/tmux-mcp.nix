{ pkgs, ... }:

let
  inherit (pkgs.lib)
    escapeShellArg
    makeBinPath
    optionalString
    optionals
    ;

  containerRuntimePath = makeBinPath (
    optionals pkgs.stdenv.isLinux [
      pkgs.podman
      pkgs.shadow
      pkgs.skopeo
      pkgs.util-linux
    ]
  );
  containerRuntimeEnvironment = optionalString pkgs.stdenv.isLinux "PATH=${escapeShellArg containerRuntimePath}:$PATH ";
  repoScript = script: ''
    run_repo_script ${escapeShellArg "tests/mcp/tmux/${script}.sh"}
  '';
  sandboxScript = script: ''
    ${containerRuntimeEnvironment}run_repo_script ${escapeShellArg "tests/mcp/tmux/${script}.sh"}
  '';
in
{
  "tmux-mcp.e2e-sandbox" = sandboxScript "e2e-sandbox";

  "tmux-mcp.integration" = repoScript "integration";
}
