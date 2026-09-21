{ pkgs }:

pkgs.runCommandLocal "test-builder-vmnet-route"
  {
    nativeBuildInputs = [
      pkgs.gawk
      pkgs.gnugrep
    ];
  }
  ''
    set -euo pipefail
    container() { printf '%s\n' "$network"; }
    route() { printf 'interface: %s\n' "$default_interface"; }
    netstat() { printf '%s\n' "$routes"; }
    ifconfig() { printf '%s\n' 'bridge100: flags' '  inet 192.168.64.1 netmask 0xffffff00'; }
    sudo() { printf '%s\n' "$*" >> additions; }

    network='[{"status":{"ipv4Subnet":"192.168.64.0/24"}}]'
    default_interface=en0
    routes=""
    : > additions
    ${import ../../lib/builder/vmnet-route.nix { inherit pkgs; }}
    [[ "$_vpn_conflict" == false && ! -s additions ]]

    default_interface=utun8
    _fix_vmnet_route
    [[ "$_vpn_conflict" == true ]]
    printf '%s\n' \
      'route add -net 192.168.64.0/25 -interface bridge100' \
      'route add -net 192.168.64.128/25 -interface bridge100' > expected
    cmp expected additions

    : > additions
    routes='192.168.64.128/25 link#2 bridge100'
    network='{"status":{"ipv4Subnet":"192.168.64.0/24"}}'
    _fix_vmnet_route
    [[ ! -s additions ]]

    network='{}'
    _fix_vmnet_route
    [[ ! -s additions ]]
    mkdir "$out"
  ''
