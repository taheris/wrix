{ pkgs }:

let
  policy = pkgs.runCommand "wrix-linux-network-policy" { nativeBuildInputs = [ pkgs.gawk ]; } ''
    awk '
      /^# BEGIN wrix network policy$/ { capture = 1 }
      capture { print }
      /^# END wrix network policy$/ { exit }
    ' ${../../lib/sandbox/linux/entrypoint.sh} > "$out"
  '';
in
pkgs.testers.runNixOSTest {
  name = "services-cache-network";
  requiredFeatures.kvm = false;

  nodes.machine = {
    networking.firewall.enable = false;
    networking.nftables.enable = true;
    environment.systemPackages = with pkgs; [
      curl
      gawk
      gnugrep
      iproute2
      nftables
      python3
    ];
  };

  testScript = ''
    machine.start()
    machine.wait_for_unit("multi-user.target")
    machine.succeed("systemctl stop dhcpcd.service")
    machine.succeed("ip netns add sandbox")
    machine.succeed("ip link add cache-host type veth peer name sandbox-peer")
    machine.succeed("ip addr add 169.254.1.2/24 dev cache-host")
    machine.succeed("ip link set cache-host up")
    machine.succeed("ip link set sandbox-peer netns sandbox")
    machine.succeed("ip netns exec sandbox ip addr add 169.254.1.10/24 dev sandbox-peer")
    machine.succeed("ip netns exec sandbox ip link set lo up")
    machine.succeed("ip netns exec sandbox ip link set sandbox-peer up")
    machine.succeed("python3 -m http.server 21042 --bind 169.254.1.2 >/tmp/cache.log 2>&1 &")
    machine.succeed("python3 -m http.server 21043 --bind 169.254.1.2 >/tmp/unrelated.log 2>&1 &")
    machine.wait_until_succeeds("curl -fsS http://169.254.1.2:21042 >/dev/null")
    machine.wait_until_succeeds("curl -fsS http://169.254.1.2:21043 >/dev/null")

    machine.succeed(
      "ip netns exec sandbox env "
      "WRIX_NETWORK=limit "
      "WRIX_NETWORK_ALLOWLIST= "
      "WRIX_NETWORK_DNS_SERVERS= "
      "WRIX_NETWORK_LOCAL_ENDPOINTS= "
      "WRIX_PROJECT_CACHE_HOST=169.254.1.2 "
      "WRIX_PROJECT_CACHE_PORT=21042 "
      "WRIX_FIREWALL_BACKEND=nft "
      "WRIX_NFT_BIN=${pkgs.nftables}/bin/nft "
      "WRIX_GETENT_BIN=${pkgs.glibc.bin}/bin/getent "
      "WRIX_AWK_BIN=${pkgs.gawk}/bin/awk "
      "WRIX_SORT_BIN=${pkgs.coreutils}/bin/sort "
      "WRIX_GREP_BIN=${pkgs.gnugrep}/bin/grep "
      "${pkgs.bash}/bin/bash -c 'source ${policy}; apply_wrix_network_policy'"
    )
    machine.succeed(
      "ip netns exec sandbox curl --connect-timeout 2 -fsS "
      "http://169.254.1.2:21042 >/dev/null"
    )
    machine.fail(
      "ip netns exec sandbox curl --connect-timeout 2 -fsS "
      "http://169.254.1.2:21043 >/dev/null"
    )
  '';
}
