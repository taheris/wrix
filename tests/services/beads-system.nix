{
  pkgs,
  wrix,
  beadsImage,
}:

let
  inherit (pkgs.lib) escapeShellArg makeBinPath;

  serviceImage = import ../../lib/services/image.nix {
    inherit pkgs;
    inherit (wrix.rustPackage) cacheServe;
  };
  beadsShell = wrix.mkDevShell {
    profile = wrix.profiles.base;
    nixCache = false;
    prekHooks = false;
  };
  beadsHook = pkgs.writeText "wrix-beads-system-shell-hook" beadsShell.shellHook;
  profileConfigBase = pkgs.writeText "wrix-beads-system-profile-base.json" (
    builtins.toJSON {
      schema = 1;
      system = pkgs.stdenv.hostPlatform.system;
      profile = {
        name = "beads-system";
        env = { };
        mounts = [ ];
        writable_dirs = [ ];
        network_allowlist = [ ];
      };
      image = {
        ref = "localhost/wrix-beads-system:latest";
        source = "${beadsImage.source}";
        inherit (beadsImage) source_kind;
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
        nix_cache.enable = false;
      };
      features.mcp_runtime = false;
    }
  );
  profileConfig =
    pkgs.runCommand "wrix-beads-system-profile.json" { nativeBuildInputs = [ pkgs.jq ]; }
      ''
        set -euo pipefail
        jq --arg digest "$(cat ${beadsImage.digest})" '.image.digest = $digest' \
          ${profileConfigBase} > "$out"
      '';
  testPath = makeBinPath (
    with pkgs;
    [
      bash
      beads
      coreutils
      dolt
      findutils
      git
      gnugrep
      jq
      openssh
      podman
      skopeo
      systemd
      wrix.rustPackage.wrix
    ]
  );
  commonEnvironment = ''
    export PATH=/run/wrappers/bin:/run/current-system/sw/bin:${testPath}
    export HOME=/home/alice
    export XDG_STATE_HOME="$HOME/.local/state"
    export XDG_CACHE_HOME="$HOME/.cache"
    export BD_NON_INTERACTIVE=1
    export WRIX_CONTAINER_RUNTIME=podman
    export WRIX_SERVICE_IMAGE=${escapeShellArg serviceImage.ref}
    export WRIX_SERVICE_IMAGE_SOURCE=${escapeShellArg "${serviceImage.source}"}
    export WRIX_SERVICE_IMAGE_SOURCE_KIND=${escapeShellArg serviceImage.source_kind}
    export WRIX_SERVICE_IMAGE_DIGEST=${escapeShellArg "${serviceImage.digest}"}
  '';
  fixtureSetup = pkgs.writeShellScript "wrix-beads-system-fixture" ''
    set -euo pipefail
    ${commonEnvironment}

    repo="$HOME/beads-repo"
    git -C "$repo" init -q
    git -C "$repo" config user.name "Wrix Test"
    git -C "$repo" config user.email "wrix@example.invalid"
    mkdir -p \
      "$repo/.beads/dolt" \
      "$repo/.git/beads-worktrees/beads/.beads/dolt-remote" \
      "$repo/.wrix" \
      "$XDG_STATE_HOME" \
      "$XDG_CACHE_HOME"
    chmod 700 "$repo/.beads"
    (
      cd "$repo/.beads/dolt"
      dolt init --name "Wrix Test" --email "wrix@example.invalid" >/dev/null
    )
  '';
  beadsParent = pkgs.writeShellScript "wrix-beads-system-parent" ''
    set -euo pipefail
    ${commonEnvironment}

    cd "$HOME/beads-repo"
    source ${beadsHook}
    printf 'socket=%s\nauto=%s\n' \
      "''${BEADS_DOLT_SERVER_SOCKET:-}" \
      "''${BEADS_DOLT_AUTO_START:-}" \
      > "$HOME/beads-hook.env"
    touch "$HOME/beads-hook.ready"
    exec sleep infinity
  '';
  prepareSync = pkgs.writeShellScript "wrix-beads-system-prepare-sync" ''
    set -euo pipefail
    ${commonEnvironment}

    repo="$HOME/beads-repo"
    remote="$repo/.git/beads-worktrees/beads/.beads/dolt-remote"
    socket=$(cd "$repo" && wrix service dolt socket)
    export BEADS_DOLT_SERVER_SOCKET="$socket"
    export BEADS_DOLT_AUTO_START=0

    cd "$repo"
    bd init \
      --prefix wx \
      --skip-hooks \
      --skip-agents \
      --non-interactive \
      --server \
      --server-socket "$socket" \
      --database wx \
      >/dev/null
    chmod 700 .beads
    bd config set export.auto false >/dev/null
    bd dolt remote add origin "file://$remote" >/dev/null
    bd dolt commit >/dev/null
    bd dolt push >/dev/null

    remote_digest() {
      find "$remote" -type f -printf '%P:%s\n' | sort | sha256sum | cut -d' ' -f1
    }

    before=$(remote_digest)
    bd create --title "sandbox sync probe" --type task --silent >/dev/null
    bd dolt commit >/dev/null
    if [[ "$(remote_digest)" != "$before" ]]; then
      printf 'remote changed before sandbox push\n' >&2
      exit 1
    fi
    printf '%s\n' "$before" > "$HOME/beads-remote.before"

    bd sql "CALL DOLT_REMOTE('remove', 'origin')" >/dev/null
    bd sql "CALL DOLT_REMOTE('add', 'origin', 'file:///host-only/beads/dolt-remote')" >/dev/null
    bd dolt remote list | grep -F 'file:///host-only/beads/dolt-remote' >/dev/null

    ssh-keygen -t ed25519 -N "" -q -f "$HOME/deploy-key" -C "wrix-system-test" >/dev/null
    wrix service stop >/dev/null
  '';
  sandboxSync = pkgs.writeShellScript "wrix-beads-system-sandbox-sync" ''
    set -euo pipefail
    ${commonEnvironment}

    repo="$HOME/beads-repo"
    command='[[ "''${BEADS_DOLT_AUTO_START:-}" == "0" ]] && [[ -S "''${BEADS_DOLT_SERVER_SOCKET:-}" ]] && [[ ! -e .beads/issues.jsonl ]] && bd dolt pull && bd dolt push'
    jq -n \
      --arg workspace "$repo" \
      --arg command "$command" \
      '{workspace:$workspace,env:[],agent_args:["bash","-euo","pipefail","-c",$command],mounts:[]}' \
      > "$HOME/spawn.json"

    export WRIX_DEPLOY_KEY="$HOME/deploy-key"
    export WRIX_GIT_SIGN=0
    ${wrix.rustPackage.wrix}/bin/wrix \
      --profile-config ${profileConfig} \
      spawn --spawn-config "$HOME/spawn.json"
  '';
  verifySync = pkgs.writeShellScript "wrix-beads-system-verify-sync" ''
    set -euo pipefail
    ${commonEnvironment}

    repo="$HOME/beads-repo"
    remote="$repo/.git/beads-worktrees/beads/.beads/dolt-remote"
    after=$(find "$remote" -type f -printf '%P:%s\n' | sort | sha256sum | cut -d' ' -f1)
    before=$(<"$HOME/beads-remote.before")
    if [[ "$after" == "$before" ]]; then
      printf 'sandbox push did not update the real Dolt remote\n' >&2
      exit 1
    fi

    socket=$(cd "$repo" && wrix service dolt socket)
    export BEADS_DOLT_SERVER_SOCKET="$socket"
    export BEADS_DOLT_AUTO_START=0
    cd "$repo"
    wrix service dolt wait >/dev/null
    bd dolt remote list | grep -F 'file:///host-only/beads/dolt-remote' >/dev/null
  '';
in
pkgs.testers.runNixOSTest {
  name = "beads-live-system";
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
    machine.succeed("mkdir -p /home/alice/beads-repo")
    machine.succeed("chown -R alice:users /home/alice")
    machine.succeed(as_alice("${fixtureSetup}"))

    with subtest("beads shellHook service survives its systemd parent"):
        machine.succeed(
            as_alice(
                "systemd-run --user --unit=wrix-beads-parent "
                "--property=Type=exec --property=TimeoutStopSec=8s ${beadsParent}"
            )
        )
        machine.wait_until_succeeds("test -e /home/alice/beads-hook.ready", timeout=90)
        machine.succeed("grep -F 'auto=0' /home/alice/beads-hook.env")
        machine.succeed(
            as_alice(
                "podman inspect --format '{{.State.Running}}' beads-repo-service | grep true"
            )
        )
        machine.succeed(
            as_alice("timeout 3s systemctl --user stop wrix-beads-parent.service")
        )
        machine.sleep(1)
        machine.succeed(
            as_alice(
                "podman inspect --format '{{.State.Running}}' beads-repo-service | grep true"
            )
        )

    with subtest("live sandbox uses the shared service for real Dolt sync"):
        machine.succeed(as_alice("${prepareSync}"), timeout=120)
        machine.fail(as_alice("podman container exists beads-repo-service"))
        machine.succeed(as_alice("${sandboxSync}"), timeout=300)
        machine.succeed(
            as_alice(
                "podman inspect --format '{{.State.Running}}' beads-repo-service | grep true"
            )
        )
        machine.succeed(as_alice("${verifySync}"), timeout=60)

    machine.succeed(
        as_alice(
            "cd /home/alice/beads-repo && ${wrix.rustPackage.wrix}/bin/wrix service stop"
        )
    )
    machine.fail(as_alice("podman container exists beads-repo-service"))
  '';
}
