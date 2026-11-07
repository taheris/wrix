{
  pkgs,
  linuxPkgs,
  wrix,
  src,
}:
let
  inherit (pkgs.lib) getExe;
  runner = import ../sandbox/fixtures/command-runner.nix { pkgs = linuxPkgs; };
  sandbox = wrix.mkSandbox {
    agent = "direct";
    agentPkg = runner;
  };
  runtimeManifest = (wrix.mkSandbox { mcpRuntime = true; }).image.mcpAvailableJson;
  guidanceHarness = import ../../lib/util/rust-tool.nix {
    inherit pkgs;
    package = "wrix-verify-tools";
    description = "Native executable tmux guidance conformance";
    nativeCheckInputs = [
      pkgs.bash
      pkgs.tmux
      pkgs.curl
      # The guide intentionally runs upstream Python's HTTP server example.
      pkgs.python3
    ];
  };
in
{
  inherit sandbox;
  checks.tmux-retired-selection-rejected =
    pkgs.runCommand "tmux-retired-selection-rejected"
      {
        nativeBuildInputs = [
          pkgs.bash
          pkgs.jq
          pkgs.coreutils
          pkgs.gnused
          pkgs.findutils
        ];
      }
      ''
        set -euo pipefail
        export REPO_ROOT=${src}
        export WRIX_TEST_MCP_AVAILABLE=${runtimeManifest}
        bash "$REPO_ROOT/tests/sandbox/entrypoint-contract.sh" test_runtime_fixtures_do_not_need_env_or_path
        bash "$REPO_ROOT/tests/sandbox/entrypoint-contract.sh" test_retired_tmux_selection_rejected
        touch "$out"
      '';
  checks.tmux-guidance-commands = guidanceHarness.overrideAttrs {
    pname = "tmux-guidance-commands";
    cargoTestFlags = [
      "--package"
      "wrix-verify-tools"
      "--test"
      "tmux_guidance"
    ];
    checkFlags = [
      "--ignored"
      "--exact"
      "documented_commands_support_native_debugging"
    ];
    WRIX_TEST_TMUX_GUIDE = ../../docs/tmux.md;
  };
  checks.tmux-workflow-syntax =
    pkgs.runCommand "tmux-workflow-syntax"
      {
        nativeBuildInputs = [ pkgs.shellcheck ];
      }
      ''
        set -euo pipefail
        cd ${src}
        ${getExe pkgs.shellcheck} --external-sources tests/tmux/workflow.sh tests/judges/tmux.sh
        touch "$out"
      '';
}
