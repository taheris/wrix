{ pkgs }:

pkgs.writeShellScriptBin "test-command-runner" ''
  set -euo pipefail
  if [[ "$#" -gt 0 ]]; then
    exec "$@"
  fi
  probe="''${WRIX_TEST_PROBE_WORKSPACE:-/workspace}/bin/test-agent-probe"
  if [[ -x "$probe" ]]; then
    exec "$probe"
  fi
''
