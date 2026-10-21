{
  pkgs,
  wrix,
}:

let
  inherit (pkgs.lib) escapeShellArg makeBinPath;

  defaultShell = wrix.mkDevShell {
    profile = wrix.profiles.base;
    prekHooks = false;
  };
  disabledShell = wrix.mkDevShell {
    profile = wrix.profiles.base;
    nixCache = false;
    prekHooks = false;
  };
  defaultHook = pkgs.writeText "wrix-default-devshell-hook" defaultShell.shellHook;
  disabledHook = pkgs.writeText "wrix-disabled-cache-devshell-hook" disabledShell.shellHook;
  testPath = makeBinPath [
    pkgs.coreutils
    pkgs.git
    pkgs.gnugrep
    pkgs.nix
    pkgs.podman
    pkgs.skopeo
    pkgs.systemd
    wrix.rustPackage.wrix
  ];
  defaultParent = pkgs.writeShellScript "wrix-default-devshell-parent" ''
    set -euo pipefail
    export PATH=/run/wrappers/bin:/run/current-system/sw/bin:${testPath}
    export HOME=/home/alice
    export XDG_STATE_HOME="$HOME/.local/state"
    export XDG_CACHE_HOME="$HOME/.cache"
    export NIX_CONFIG="experimental-features = nix-command flakes"
    export WRIX_CONTAINER_RUNTIME=podman
    export WRIX_NIX_CACHE_REQUIRE_TRUSTED=0
    export WRIX_SERVICE_ALLOW_TEMP_CACHE=1
    export WRIX_SERVICE_IMAGE=${escapeShellArg defaultShell.WRIX_SERVICE_IMAGE}
    export WRIX_SERVICE_IMAGE_SOURCE=${escapeShellArg defaultShell.WRIX_SERVICE_IMAGE_SOURCE}
    export WRIX_SERVICE_IMAGE_SOURCE_KIND=${escapeShellArg defaultShell.WRIX_SERVICE_IMAGE_SOURCE_KIND}
    export WRIX_SERVICE_IMAGE_DIGEST=${escapeShellArg defaultShell.WRIX_SERVICE_IMAGE_DIGEST}
    cd /home/alice/devshell-repo
    source ${defaultHook}
    ${pkgs.nix}/bin/nix config show --json > /home/alice/default-host-nix-config.json
    mkfifo -m 600 /home/alice/devshell-parent.lifetime
    exec 3<>/home/alice/devshell-parent.lifetime
    touch /home/alice/default-hook.ready
    exec ${pkgs.coreutils}/bin/cat <&3
  '';
  disabledProbe = pkgs.writeShellScript "wrix-disabled-cache-devshell-probe" ''
    set -euo pipefail
    export PATH=/run/wrappers/bin:/run/current-system/sw/bin:${testPath}
    export HOME=/home/alice
    cd /home/alice/disabled-repo
    source ${disabledHook}
    touch /home/alice/disabled-hook.ready
  '';
in
pkgs.testers.runNixOSTest {
  name = "services-devshell-start-independent";
  requiredFeatures.kvm = false;

  nodes.machine = {
    imports = [ (import ../lib/userspace-vm.nix { inherit pkgs; }) ];
    users.users.alice = {
      isNormalUser = true;
      uid = 1000;
    };
    virtualisation = {
      cores = 2;
      memorySize = 3072;
      podman.enable = true;
    };
  };

  testScript = ''
    from datetime import timedelta
    import json
    ${builtins.readFile ../lib/guest-store.py}

    def as_alice(command: str) -> str:
        environment = (
            "export XDG_RUNTIME_DIR=/run/user/1000; "
            "export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus; "
        )
        return f"su alice -l -c {shlex.quote(environment + command)}"

    def owner_pid() -> str:
        pid = machine.succeed(as_alice(
            "podman inspect --format '{{.State.Pid}}' devshell-repo-service"
        )).strip()
        assert pid.isdigit() and int(pid) > 0
        return pid

    def owner_key_identity() -> str:
        return machine.succeed(as_alice(
            "set -euo pipefail; cd /home/alice/devshell-repo; "
            "state=$(${wrix.rustPackage.wrix}/bin/wrix service endpoints | ${pkgs.jq}/bin/jq -er .state_root); "
            "stat -c '%i:%s' \"$state/keys/cache.pub\"; "
            "sha256sum \"$state/keys/cache.pub\""
        ))

    machine.start()
    machine.wait_for_unit("multi-user.target")
    register_guest_store(machine, "${pkgs.nix}/bin/nix-store", ["${defaultParent}", "${disabledProbe}"])
    machine.succeed("loginctl enable-linger alice")
    machine.succeed(as_alice("whoami"))
    machine.wait_for_unit("user@1000.service")
    machine.succeed(as_alice("systemctl --user show-environment"))
    machine.succeed(
        "mkdir -p /home/alice/devshell-repo/.git /home/alice/disabled-repo/.git "
        "/home/alice/disabled-repo/.beads/dolt /home/alice/.local/state /home/alice/.cache"
    )
    machine.succeed("chown -R alice:users /home/alice")

    with subtest("default shell starts in an independent scope"):
        machine.succeed(
            as_alice(
                "systemd-run --user --unit=wrix-devshell-parent "
                "--property=Type=exec --property=TimeoutStopSec=8s ${defaultParent}"
            )
        )
        try:
            machine.wait_until_succeeds(
                "test -e /home/alice/default-hook.ready || "
                + as_alice("systemctl --user is-failed --quiet wrix-devshell-parent.service"),
                timeout=timedelta(minutes=15),
            )
            machine.succeed("test -e /home/alice/default-hook.ready")
        except Exception:
            print(machine.execute(as_alice(
                "journalctl --user -u wrix-devshell-parent.service --no-pager -n 80"
            ), timeout=timedelta(seconds=15))[1])
            raise
        config = json.loads(machine.succeed("cat /home/alice/default-host-nix-config.json"))
        endpoints = json.loads(machine.succeed(as_alice(
            "cd /home/alice/devshell-repo; ${wrix.rustPackage.wrix}/bin/wrix service endpoints"
        )))
        public_key = machine.succeed("cat " + shlex.quote(endpoints["state_root"] + "/keys/cache.pub")).strip()
        assert public_key not in config["trusted-public-keys"]["value"]
        for substituter in config["substituters"]["value"]:
            assert endpoints["cache_root"] not in substituter
            assert ":" + str(endpoints["endpoints"]["cache_http"]["port"]) not in substituter
        assert config["post-build-hook"]["value"].endswith("/bin/wrix-cache-post-build-hook")
        machine.succeed(
            as_alice(
                "podman inspect --format '{{.State.Running}}' devshell-repo-service | grep true"
            )
        )
        machine.succeed(
            as_alice(
                "systemctl --user list-units --type=scope --state=running --no-legend "
                "'run-*.scope' | grep 'run-.*[.]scope'"
            )
        )

    serving_pid = owner_pid()
    serving_key = owner_key_identity()

    with subtest("stopping the caller is prompt and leaves the container running"):
        machine.succeed(
            as_alice(
                "timeout 3s systemctl --user stop wrix-devshell-parent.service"
            )
        )
        machine.fail(as_alice("systemctl --user is-active --quiet wrix-devshell-parent.service"))
        machine.succeed(
            as_alice(
                "podman inspect --format '{{.State.Running}}' devshell-repo-service | grep true"
            )
        )

    with subtest("cache opt-out still starts the beads service"):
        machine.succeed(as_alice("${disabledProbe}"))
        machine.succeed("test -e /home/alice/disabled-hook.ready")
        machine.succeed(as_alice("podman container exists disabled-repo-service"))

    machine.succeed(
        as_alice(
            "cd /home/alice/devshell-repo && ${wrix.rustPackage.wrix}/bin/wrix service stop"
        )
    )
    assert owner_pid() == serving_pid
    assert owner_key_identity() == serving_key
    machine.succeed(as_alice(
        "cd /home/alice/devshell-repo && "
        "${wrix.rustPackage.wrix}/bin/wrix service status | grep -Fx 'runtime: Stopped'"
    ))
    machine.succeed(as_alice(
        "cd /home/alice/devshell-repo && ${wrix.rustPackage.wrix}/bin/wrix service start"
    ))
    assert owner_pid() == serving_pid
    assert owner_key_identity() == serving_key
    machine.succeed(as_alice(
        "cd /home/alice/devshell-repo && ${wrix.rustPackage.wrix}/bin/wrix service stop"
    ))
    machine.succeed(
        as_alice(
            "cd /home/alice/disabled-repo && ${wrix.rustPackage.wrix}/bin/wrix service stop --no-cache"
        )
    )
    machine.fail(as_alice("podman container exists disabled-repo-service"))
    assert owner_pid() == serving_pid
    assert owner_key_identity() == serving_key
  '';
}
