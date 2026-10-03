{ pkgs }:

let
  runtimeInputs = [
    pkgs.prek
    pkgs.git
    pkgs.coreutils
  ];
in
pkgs.writeShellApplication {
  name = "wrix-prek";
  inherit runtimeInputs;
  text = ''
    set -euo pipefail
    ${builtins.readFile ./binding.sh}
    wrix_prek_check_runtime() {
      local executable
      for executable in ${pkgs.prek}/bin/prek ${pkgs.git}/bin/git ${pkgs.coreutils}/bin/{cat,dirname,mkdir,rm,uname}; do
        [[ -x "$executable" ]] || wrix_prek_fail "packaged hook runtime dependency $executable is missing"
      done
    }
    if [[ "''${1:-}" = "--print-bin-dir" ]]; then
      wrix_prek_check_runtime
      printf '%s\n' "${pkgs.lib.makeBinPath runtimeInputs}"
      exit 0
    fi
    if [[ "''${1:-}" = "--bind" ]]; then
      wrix_prek_check_runtime
      key=$(wrix_prek_binding_key)
      [[ "$key" == *-${pkgs.stdenv.hostPlatform.system}.runner ]] || wrix_prek_fail "runner does not match the executing platform"
      runner="$(cd -- "''${BASH_SOURCE[0]%/*}" && pwd -P)/wrix-prek"
      if current=$(wrix_prek_config_get "$key"); then
        if [[ "$current" == "$runner" ]]; then
          exit 0
        fi
      else
        status="$?"
        [[ "$status" == 1 ]] || wrix_prek_fail "cannot read repository-local Git config $key (exit $status)"
      fi
      git config --local "$key" "$runner"
      [[ "$(wrix_prek_config_get "$key")" == "$runner" ]] || wrix_prek_fail "runner binding verification failed for $key"
      exit 0
    fi
    exec prek "$@"
  '';
}
