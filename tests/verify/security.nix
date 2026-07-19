{ pkgs, ... }:

let
  inherit (pkgs.lib) escapeShellArg;

  securityScript = script: ''
    run_repo_script ${escapeShellArg "tests/security/${script}.sh"}
  '';
  securityScriptFunction = script: function: ''
    run_repo_script ${escapeShellArg "tests/security/${script}.sh"} ${escapeShellArg function}
  '';
  sandboxScriptWithWrix = script: function: ''
    run_repo_script_with_wrix ${escapeShellArg "tests/sandbox/${script}.sh"} ${escapeShellArg function}
  '';
in
{
  "security.audit-trail-anchor" = securityScript "audit-trail-anchor";

  "security.git-ssh-bootstrap" =
    securityScriptFunction "git-ssh-bootstrap" "test_fresh_container_git_ssh_bootstrap";

  "security.host-container-loom-git-helper" =
    securityScriptFunction "git-ssh-bootstrap" "test_host_container_and_loom_helper";

  "security.nested-key-propagation" = securityScript "nested-key-propagation";

  "security.provider-credential-env" =
    sandboxScriptWithWrix "rust-launcher-live" "test_linux_host_provider_credentials_reach_live_launcher";
}
