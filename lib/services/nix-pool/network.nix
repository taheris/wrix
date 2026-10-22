{ pkgs }:

let
  object =
    pkgs.runCommand "nix-pool-egress.o"
      {
        nativeBuildInputs = [ pkgs.llvmPackages.clang-unwrapped ];
      }
      ''
        clang -target bpf -O2 -g -Wall -Wextra -Werror \
          -I${pkgs.linuxHeaders}/include -I${pkgs.lib.getDev pkgs.libbpf}/include \
          -c ${./network.c} -o "$out"
      '';
  install = pkgs.writeShellScriptBin "wrix-nix-pool-egress" ''
    set -euo pipefail
    [[ "$#" == 0 && $(${pkgs.coreutils}/bin/id -u) == 0 ]]
    [[ -f /etc/wrix/nix-pool-service && ! -e /etc/wrix/image-agent ]]
    [[ $(</etc/wrix/nix-pool-service) == 1 ]]
    ${pkgs.coreutils}/bin/mkdir -p /sys/fs/cgroup /sys/fs/bpf
    if ! ${pkgs.util-linux}/bin/mountpoint -q /sys/fs/cgroup; then
      ${pkgs.util-linux}/bin/mount -t cgroup2 cgroup2 /sys/fs/cgroup
    fi
    if ! ${pkgs.util-linux}/bin/mountpoint -q /sys/fs/bpf; then
      ${pkgs.util-linux}/bin/mount -t bpf bpf /sys/fs/bpf
    fi
    [[ ! -e /sys/fs/bpf/wrix-nix-pool ]]
    ${pkgs.bpftools}/bin/bpftool prog loadall ${object} /sys/fs/bpf/wrix-nix-pool
    ${pkgs.bpftools}/bin/bpftool cgroup attach /sys/fs/cgroup connect4 pinned /sys/fs/bpf/wrix-nix-pool/connect4
    ${pkgs.bpftools}/bin/bpftool cgroup attach /sys/fs/cgroup connect6 pinned /sys/fs/bpf/wrix-nix-pool/connect6
    ${pkgs.bpftools}/bin/bpftool cgroup attach /sys/fs/cgroup sendmsg4 pinned /sys/fs/bpf/wrix-nix-pool/sendmsg4
    ${pkgs.bpftools}/bin/bpftool cgroup attach /sys/fs/cgroup sendmsg6 pinned /sys/fs/bpf/wrix-nix-pool/sendmsg6
  '';
in
{
  inherit object install;
}
