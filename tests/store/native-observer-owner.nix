{ pkgs, owner }:
let
  boundary = pkgs.replaceVars ./native-boundary.py {
    bpftool = "${pkgs.bpftools}/bin/bpftool";
  };
in

# Qualification-only instrumentation. Production owner logic, SSH authorization,
# kernel, runtime and resource policy are unchanged; no arbitrary exec is exposed.
pkgs.writeShellScriptBin "wrix-nix-pool-owner" ''
  set -euo pipefail
  if [[ "$#" == 1 && "$1" == control && "$EUID" == 0 ]]; then
    case "''${SSH_ORIGINAL_COMMAND:-}" in
      wrix-native-observe-boot-id)
        exec ${pkgs.python3}/bin/python3 ${boundary} boot-id ;;
      wrix-native-observe-serve)
        exec ${pkgs.python3}/bin/python3 ${boundary} serve ;;
      wrix-native-observe-snapshot)
        exec ${pkgs.python3}/bin/python3 ${boundary} snapshot ;;
      wrix-native-observe-stop)
        exec ${pkgs.python3}/bin/python3 ${boundary} stop ;;
      wrix-native-observe-kill-daemon)
        exec ${pkgs.python3}/bin/python3 ${boundary} kill-daemon ;;
    esac
  fi
  exec ${owner}/bin/wrix-nix-pool-owner "$@"
''
