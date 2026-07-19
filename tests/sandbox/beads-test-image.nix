{ pkgs }:

let
  packages = with pkgs; [
    bash
    beads
    coreutils
    dolt
    gawk
    getent.provider
    git
    gnugrep
    gnused
    iproute2
    iptables
    jq
    libcap
    netcat
    openssh
    procps
    util-linux
    yq
  ];
  profile = {
    name = "beads-system";
    corePackages = packages;
    inherit packages;
    env = { };
    mounts = [ ];
    networkAllowlist = [ ];
    writableDirs = [ ];
  };
in
import ../../lib/sandbox/image.nix {
  inherit pkgs profile;
  agent = "direct";
  agentPkg = pkgs.hello;
  entrypointSh = ../../lib/sandbox/linux/entrypoint.sh;
  krunSupport = false;
  claudeConfig = { };
  claudeSettings = { };
}
