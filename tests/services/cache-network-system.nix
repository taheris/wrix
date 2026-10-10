{
  pkgs,
  wrix,
  sandboxImage,
}:

let
  inherit (pkgs.lib) escapeShellArg makeBinPath;

  serviceImage = import ../../lib/services/image.nix {
    inherit pkgs;
    inherit (wrix.rustPackage) cacheServe;
  };
  profileConfigBase = pkgs.writeText "wrix-cache-network-system-profile-base.json" (
    builtins.toJSON {
      schema = 1;
      system = pkgs.stdenv.hostPlatform.system;
      profile = {
        name = "cache-network-system";
        env = { };
        mounts = [ ];
        writable_dirs = [ ];
        network_allowlist = [ ];
      };
      image = {
        ref = "localhost/wrix-cache-network-system:latest";
        source = "${sandboxImage.source}";
        inherit (sandboxImage) source_kind;
        digest = "";
      };
      agent.kind = "direct";
      resources = {
        cpus = null;
        memory_mb = 2048;
        pids_limit = 2048;
      };
      security.deploy_key = null;
      network = {
        default_mode = "open";
        ipv6 = "disabled";
      };
      services = {
        beads.enable = "auto";
        nix_cache.enable = true;
      };
      features.mcp_runtime = false;
    }
  );
  profileConfig =
    pkgs.runCommand "wrix-cache-network-system-profile.json" { nativeBuildInputs = [ pkgs.jq ]; }
      ''
        set -euo pipefail
        jq --arg digest "$(cat ${sandboxImage.digest})" '.image.digest = $digest' \
          ${profileConfigBase} > "$out"
      '';
  testPath = makeBinPath (
    with pkgs;
    [
      bash
      coreutils
      curl
      git
      gnugrep
      jq
      nix
      openssh
      podman
      skopeo
      systemd
      util-linux
      wrix.rustPackage.wrix
    ]
  );
  commonEnvironment = ''
    export PATH=/run/wrappers/bin:/run/current-system/sw/bin:${testPath}
    export HOME=/home/alice
    export XDG_STATE_HOME="$HOME/.local/state"
    export XDG_CACHE_HOME="$HOME/.cache"
    export WRIX_CONTAINER_RUNTIME=podman
    export WRIX_SERVICE_ALLOW_TEMP_CACHE=1
    export WRIX_SERVICE_IMAGE=${escapeShellArg serviceImage.ref}
    export WRIX_SERVICE_IMAGE_SOURCE=${escapeShellArg "${serviceImage.source}"}
    export WRIX_SERVICE_IMAGE_SOURCE_KIND=${escapeShellArg serviceImage.source_kind}
    export WRIX_SERVICE_IMAGE_DIGEST=${escapeShellArg "${serviceImage.digest}"}
  '';
  fixtureSetup = pkgs.writeShellScript "wrix-cache-network-system-fixture" ''
    set -euo pipefail
    ${commonEnvironment}

    repo="$HOME/cache-network-repo"
    mkdir -p "$repo" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"
    git -C "$repo" init -q -b main
  '';
  unrelatedListener = pkgs.writeShellScript "wrix-cache-network-unrelated-listener" ''
    set -euo pipefail
    exec ${pkgs.python3}/bin/python3 -m http.server 29999 --bind 127.0.0.1
  '';
  sandboxProbe = pkgs.writeShellScript "wrix-cache-network-system-probe" ''
    set -euo pipefail
    ${commonEnvironment}

    repo="$HOME/cache-network-repo"
    cd "$repo"
    wrix service start
    wrix service endpoints > "$HOME/endpoints.json"
    state_root=$(jq -r .state_root "$HOME/endpoints.json")
    cache_root=$(jq -r .cache_root "$HOME/endpoints.json")
    cache_port=$(jq -r .endpoints.cache_http.port "$HOME/endpoints.json")
    [[ "$(jq -r .endpoints.cache_http.host "$HOME/endpoints.json")" == 127.0.0.1 ]]
    (( cache_port >= 21000 && cache_port <= 22999 ))
    cp "$state_root/keys/cache.pub" "$repo/expected-key"
    printf '%s\n' "$cache_port" > "$repo/expected-cache-port"
    printf '%s\n' "$state_root" > "$repo/state-root"
    printf '%s\n' "$cache_root" > "$repo/cache-root"
    printf 'service cache payload\n' > "$cache_root/nar/probe.nar"
    printf 'host-only store sentinel\n' > "$HOME/host-sentinel"
    nix-store --add "$HOME/host-sentinel" > "$repo/host-store-path"
    podman inspect cache-network-repo-service | jq -e --arg cache "$cache_root" '
      .[0].Mounts | any(.Source == $cache and .Destination == "/cache" and .RW == false)
    ' >/dev/null
    cat > "$repo/probe.sh" <<'PROBE'
    #!/usr/bin/env bash
    set -euo pipefail
    cache_url=$(awk -F' = ' '$1 == "extra-substituters" { print $2; exit }' <<<"$NIX_CONFIG")
    [[ "$cache_url" == "http://169.254.1.2:$(cat /workspace/expected-cache-port)" ]]
    [[ "$WRIX_PROJECT_CACHE_HOST" == 169.254.1.2 && "$WRIX_PROJECT_CACHE_PORT" == "$(cat /workspace/expected-cache-port)" ]]
    expected_key=$(cat /workspace/expected-key)
    expected_config=$(printf 'extra-substituters = %s\nextra-trusted-public-keys = %s\nbuilders-use-substitutes = true' "$cache_url" "$expected_key")
    [[ "$NIX_CONFIG" == "$expected_config" ]]
    [[ ! -e "$(cat /workspace/host-store-path)" ]]
    [[ ! -e /nix/var/nix/daemon-socket/socket ]]
    [[ ! -e /etc/wrix/keys/cache.secret ]]
    [[ ! -e /cache && ! -e "$(cat /workspace/state-root)" && ! -e "$(cat /workspace/cache-root)" ]]
    if awk '$5 == "/nix/store" || $5 == "/cache" { found = 1 } END { exit !found }' /proc/self/mountinfo; then
      printf 'host cache/store mount exposed to sandbox\n' >&2
      exit 1
    fi
    curl --noproxy "*" --fail --silent --show-error --connect-timeout 5 --max-time 10 "$cache_url/nix-cache-info" | grep -F "WantMassQuery: 1" >/dev/null
    [[ "$(curl --noproxy '*' -fsS --max-time 10 "$cache_url/nar/probe.nar")" == 'service cache payload' ]]
    [[ "$(curl --noproxy '*' -sS --max-time 10 -o /dev/null -w '%{http_code}' -I "$cache_url/nar/probe.nar")" == 200 ]]
    [[ "$(curl --noproxy '*' -sS --max-time 10 -o /dev/null -w '%{http_code}' -X POST "$cache_url/nar/probe.nar")" == 405 ]]
    [[ "$(curl --noproxy '*' -sS --max-time 10 -o /dev/null -w '%{http_code}' "$cache_url/")" == 404 ]]
    [[ "$(curl --noproxy '*' -sS --max-time 10 --path-as-is -o /dev/null -w '%{http_code}' "$cache_url/nar/../nix-cache-info")" == 404 ]]
    if curl --noproxy "*" --fail --silent --show-error --connect-timeout 2 --max-time 5 http://169.254.1.2:29999/ >/tmp/wrix-unrelated-service 2>&1; then
      printf "unrelated host listener reachable through cache endpoint exception\\n" >&2
      exit 1
    fi
    touch /workspace/probe-passed
    PROBE
    chmod +x "$repo/probe.sh"
    jq -n \
      --arg workspace "$repo" \
      '{workspace:$workspace,env:[],agent_args:["bash","/workspace/probe.sh"],mounts:[]}' \
      > "$HOME/spawn.json"

    export WRIX_NETWORK=limit
    ${wrix.rustPackage.wrix}/bin/wrix \
      --profile-config ${profileConfig} \
      spawn --spawn-config "$HOME/spawn.json"
    [[ -f "$repo/probe-passed" ]]
    rm "$repo/probe-passed"
    script -qefc "${wrix.rustPackage.wrix}/bin/wrix --profile-config ${profileConfig} run '$repo' bash /workspace/probe.sh" /dev/null
    [[ -f "$repo/probe-passed" ]]
    wrix service endpoints > "$HOME/endpoints-after.json"
    cmp "$HOME/endpoints.json" "$HOME/endpoints-after.json"
  '';
  stopService = pkgs.writeShellScript "wrix-cache-network-system-stop" ''
    set -euo pipefail
    ${commonEnvironment}
    cd "$HOME/cache-network-repo"
    exec ${wrix.rustPackage.wrix}/bin/wrix service stop
  '';
in
pkgs.testers.runNixOSTest {
  name = "services-limit-mode-cache-endpoint";
  requiredFeatures.kvm = false;

  nodes.machine = {
    users.users.alice = {
      isNormalUser = true;
      uid = 1000;
    };
    virtualisation = {
      cores = 2;
      diskSize = 8192;
      memorySize = 4096;
      podman.enable = true;
    };
  };

  testScript = ''
    import shlex

    def as_alice(command: str) -> str:
        environment = (
            "export XDG_RUNTIME_DIR=/run/user/1000; "
            "export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus; "
        )
        return f"su alice -l -c {shlex.quote(environment + command)}"

    machine.start()
    machine.wait_for_unit("multi-user.target")
    machine.succeed("loginctl enable-linger alice")
    machine.wait_for_unit("user@1000.service")
    machine.succeed(as_alice("systemctl --user show-environment"))
    machine.succeed("mkdir -p /home/alice")
    machine.succeed("chown -R alice:users /home/alice")
    machine.succeed(as_alice("${fixtureSetup}"))
    machine.succeed(
        as_alice(
            "systemd-run --user --unit=wrix-unrelated-listener "
            "--property=Type=exec ${unrelatedListener}"
        )
    )
    machine.wait_until_succeeds("curl -fsS http://127.0.0.1:29999 >/dev/null")

    with subtest("limit mode reaches only the assembled project cache endpoint"):
        machine.succeed(as_alice("${sandboxProbe}"), timeout=300)
        machine.succeed(
            as_alice(
                "podman inspect --format '{{.State.Running}}' "
                "cache-network-repo-service | grep true"
            )
        )

    machine.succeed(as_alice("${stopService}"))
    machine.fail(as_alice("podman container exists cache-network-repo-service"))
    machine.succeed(as_alice("systemctl --user stop wrix-unrelated-listener.service"))
  '';
}
