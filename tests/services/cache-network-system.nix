{
  pkgs,
  wrix,
  sandboxImage,
}:
let
  inherit (pkgs.lib) makeBinPath;
  first = pkgs.writeText "native-cache-first" "signed first publication\n";
  second = pkgs.writeText "native-cache-second" "signed publication after renewal\n";
  third = pkgs.writeText "native-cache-third" "signed publication after renewal\n";
  profileConfig = pkgs.writeText "native-cache-worker-profile.json" (
    builtins.toJSON {
      schema = 1;
      system = pkgs.stdenv.hostPlatform.system;
      profile = {
        name = "native-cache-worker";
        env = { };
        mounts = [ ];
        writable_dirs = [ ];
        network_allowlist = [ ];
      };
      image = {
        ref = "localhost/native-cache-worker:latest";
        source = toString sandboxImage.source;
        inherit (sandboxImage) source_kind;
      };
      agent.kind = "direct";
      resources = {
        cpus = null;
        memory_mb = 1024;
        pids_limit = 1024;
      };
      network = {
        default_mode = "limit";
        ipv6 = "disabled";
      };
      services = {
        beads.enable = "auto";
        nix_cache.enable = true;
      };
    }
  );
  testPath = makeBinPath [
    pkgs.bash
    pkgs.coreutils
    pkgs.curl
    pkgs.git
    pkgs.gnugrep
    pkgs.jq
    pkgs.nix
    pkgs.podman
    pkgs.skopeo
    pkgs.systemd
    pkgs.util-linux
    wrix.rustPackage.wrix
  ];
  environment = ''
    export PATH=/run/wrappers/bin:/run/current-system/sw/bin:${testPath}
    export HOME=/home/alice
    export XDG_STATE_HOME="$HOME/.local/state"
    export XDG_CACHE_HOME="$HOME/.cache"
    export WRIX_CONTAINER_RUNTIME=podman
    export WRIX_SERVICE_ALLOW_TEMP_CACHE=1
    export NIX_CONFIG=$'experimental-features = nix-command flakes\nsubstituters = '
    cd "$HOME/cache-network-repo"
  '';
  agent = pkgs.writeText "native-cache-worker-probe.sh" ''
    #!/usr/bin/env bash
    set -euo pipefail
    netrc=/run/wrix/cache-credentials/netrc
    first=$(cat /workspace/first-path)
    second=$(cat /workspace/second-path)
    [[ ! -e "$first" && ! -e "$second" ]]
    [[ ! -e /nix/var/nix/daemon-socket/socket && ! -e /cache ]]
    nix config show --json > /workspace/effective-config.json
    common=$(jq -er '.substituters.value[] | select(endswith("/wrix-common-v1"))' /workspace/effective-config.json)
    private=$(jq -er '.substituters.value[] | select(test("/wrix-[A-Za-z0-9_-]{43}$"))' /workspace/effective-config.json)
    [[ $(jq -r '."netrc-file".value' /workspace/effective-config.json) == "$netrc" ]]
    [[ $(jq -r '."narinfo-cache-negative-ttl".value' /workspace/effective-config.json) == 0 ]]
    [[ -r "$netrc" ]]
    if touch /run/wrix/cache-credentials/guest-write; then exit 81; fi
    if printf 'guest' >> "$netrc"; then exit 82; fi
    if chmod 600 "$netrc"; then exit 83; fi
    curl --noproxy '*' -fsS --max-time 10 --netrc-file "$netrc" "$common/nix-cache-info" >/dev/null
    [[ $(curl --noproxy '*' -sS --max-time 10 -o /dev/null -w '%{http_code}' "$private/nix-cache-info") == 401 ]]
    if curl --noproxy '*' -fsS --connect-timeout 2 --max-time 3 http://169.254.1.2:29999/; then exit 84; fi
    nix copy --from "$private" "$first"
    [[ $(cat "$first") == 'signed first publication' ]]
    if nix path-info --store "$private" "$second" > /workspace/expected-miss.log 2>&1; then exit 85; fi
    awk '{print $6}' "$netrc" | jq -Rr 'split(".")[1] | gsub("-";"+") | gsub("_";"/") | @base64d | fromjson | .exp' > /workspace/grant-expiry
    previous=$(sha256sum "$netrc")
    inode=$(stat -c %i "$netrc")
    mkfifo -m 600 /workspace/continue
    touch /workspace/worker-ready
    read -r phase < /workspace/continue
    [[ "$phase" == renew ]]
    deadline=$((SECONDS + 45))
    until [[ $(sha256sum "$netrc") != "$previous" ]]; do
      (( SECONDS < deadline ))
      sleep 0.1
    done
    [[ $(stat -c %i "$netrc") != "$inode" ]]
    if touch /run/wrix/cache-credentials/guest-write-after; then exit 86; fi
    nix copy --from "$private" "$second"
    [[ $(cat "$second") == 'signed publication after renewal' ]]
    touch /workspace/worker-passed
    printf 'native\000client-complete\n'
  '';
  setup = pkgs.writeShellScript "native-cache-worker-setup" ''
    set -euo pipefail
    mkdir -p /home/alice/cache-network-repo
    ${environment}
    git init -q -b main
    printf '%s\n' '${first}' > first-path
    printf '%s\n' '${second}' > second-path
    cp ${agent} probe.sh
    jq -n --arg drv '${builtins.unsafeDiscardOutputDependency first.drvPath}' --arg out '${first}' '{roots:[{name:"include.fixture",installable:"fixture",drv_path:$drv,out_paths:[$out]}]}' > "$HOME/first-roots.json"
    jq -n --arg drv '${builtins.unsafeDiscardOutputDependency second.drvPath}' --arg out '${second}' '{roots:[{name:"include.fixture",installable:"fixture",drv_path:$drv,out_paths:[$out]}]}' > "$HOME/second-roots.json"
    jq -n --arg drv '${builtins.unsafeDiscardOutputDependency third.drvPath}' --arg out '${third}' '{roots:[{name:"include.fixture",installable:"fixture",drv_path:$drv,out_paths:[$out]}]}' > "$HOME/third-roots.json"
    wrix service start
    WRIX_CACHE_ROOTS_FILE="$HOME/first-roots.json" wrix service cache publish
    jq -n --arg workspace "$PWD" '{workspace:$workspace,env:[],agent_args:["bash","/workspace/probe.sh"],mounts:[]}' > "$HOME/spawn.json"
  '';
  launch = pkgs.writeShellScript "native-cache-worker-launch" ''
    set -euo pipefail
    ${environment}
    if [[ "$1" == spawn ]]; then
      exec wrix --profile-config ${profileConfig} spawn --spawn-config "$HOME/spawn.json" --stdio
    fi
    exec script -qefc 'wrix --profile-config ${profileConfig} run /home/alice/cache-network-repo bash /workspace/probe.sh' /dev/null
  '';
  publish = pkgs.writeShellScript "native-cache-worker-publish-after-renewal" ''
    set -euo pipefail
    ${environment}
    WRIX_CACHE_ROOTS_FILE="$HOME/second-roots.json" wrix service cache publish
  '';
  listener = pkgs.writeShellScript "native-cache-unrelated-listener" ''
    set -euo pipefail
    exec ${pkgs.python3}/bin/python3 -m http.server 29999 --bind 127.0.0.1
  '';
in
pkgs.testers.runNixOSTest {
  name = "services-limit-mode-cache-endpoint";
  requiredFeatures.kvm = false;
  nodes.machine = {
    imports = [ (import ../lib/userspace-vm.nix { inherit pkgs; }) ];
    users.users.alice = {
      isNormalUser = true;
      uid = 1000;
    };
    virtualisation = {
      cores = 2;
      diskSize = 8192;
      memorySize = 3072;
      podman.enable = true;
    };
  };
  testScript = ''
    from datetime import timedelta
    import json
    ${builtins.readFile ../lib/guest-store.py}

    def as_alice(command):
        return "su alice -l -c " + shlex.quote("export XDG_RUNTIME_DIR=/run/user/1000; export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus; " + command)

    machine.start()
    machine.wait_for_unit("multi-user.target")
    register_guest_store(machine, "${pkgs.nix}/bin/nix-store", ["${setup}", "${launch}", "${publish}", "${listener}", "${wrix.serviceImage.source}", "${builtins.unsafeDiscardOutputDependency wrix.serviceImage.source.drvPath}"])
    machine.succeed("loginctl enable-linger alice; mkdir -p /home/alice; chown -R alice:users /home/alice")
    machine.wait_for_unit("user@1000.service")
    machine.succeed(as_alice("${setup}"), timeout=timedelta(minutes=15))
    machine.succeed(as_alice("systemd-run --user --unit=native-cache-unrelated ${listener}"))
    machine.wait_until_succeeds("curl -fsS http://127.0.0.1:29999 >/dev/null")

    for mode in ["spawn", "run"]:
        with subtest("native signed credentials, live directory renewal and freshness: " + mode):
            if mode == "run":
                machine.succeed(as_alice("rm -f /home/alice/cache-network-repo/worker-ready /home/alice/cache-network-repo/worker-passed /home/alice/cache-network-repo/continue; cp /home/alice/third-roots.json /home/alice/second-roots.json; printf '%s\\n' '${third}' > /home/alice/cache-network-repo/second-path"))
            machine.succeed(as_alice("systemd-run --user --unit=native-cache-worker-" + mode + " --property=Type=exec --property=StandardError=journal --property=StandardOutput=file:/home/alice/" + mode + ".protocol ${launch} " + mode))
            try:
                machine.wait_until_succeeds("test -e /home/alice/cache-network-repo/worker-ready || " + as_alice("systemctl --user is-failed --quiet native-cache-worker-" + mode), timeout=timedelta(minutes=15))
                machine.succeed("test -e /home/alice/cache-network-repo/worker-ready")
                worker = json.loads(machine.succeed(as_alice("podman inspect $(podman ps --filter ancestor=localhost/native-cache-worker:latest --format '{{.ID}}')")))[0]
                mounts = [mount for mount in worker["Mounts"] if mount["Destination"] == "/run/wrix/cache-credentials"]
                assert len(mounts) == 1 and not mounts[0]["RW"]
                staging = mounts[0]["Source"]
                machine.succeed(as_alice("test -r " + shlex.quote(staging + "/netrc")))
                expiry = int(machine.succeed("cat /home/alice/cache-network-repo/grant-expiry").strip())
                machine.succeed("date -s @" + str(expiry - 120))
                machine.succeed(as_alice("${publish}"), timeout=timedelta(minutes=5))
                machine.succeed(as_alice("printf 'renew\\n' > /home/alice/cache-network-repo/continue"))
                machine.wait_until_succeeds("test -e /home/alice/cache-network-repo/worker-passed || " + as_alice("systemctl --user is-failed --quiet native-cache-worker-" + mode), timeout=timedelta(seconds=90))
                machine.succeed("test -e /home/alice/cache-network-repo/worker-passed")
                machine.wait_until_succeeds(as_alice("test \"$(systemctl --user show -p ActiveState --value native-cache-worker-" + mode + ")\" = inactive"))
                machine.succeed(as_alice("test \"$(systemctl --user show -p ExecMainStatus --value native-cache-worker-" + mode + ")\" = 0"))
                machine.fail(as_alice("test -e " + shlex.quote(staging)))
                if mode == "spawn":
                    machine.succeed("printf 'native\\000client-complete\\n' | cmp - /home/alice/spawn.protocol")
            except Exception:
                print(machine.execute(as_alice("journalctl --user -u native-cache-worker-" + mode + " -u wrix-host-controller --no-pager -n 100"))[1])
                raise
  '';
}
