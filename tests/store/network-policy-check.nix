{ pkgs }:
pkgs.runCommand "native-pool-network-policy"
  {
    nativeBuildInputs = [ pkgs.stdenv.cc ];
  }
  ''
    set -euo pipefail
    cat ${../../lib/services/nix-pool/network.c} ${./network-policy-test.c} > policy.c
    cc -O2 -Wall -Wextra -Werror \
      -I${pkgs.linuxHeaders}/include -I${pkgs.lib.getDev pkgs.libbpf}/include \
      policy.c -o policy
    ./policy
    touch "$out"
  ''
