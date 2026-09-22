{
  pkgs,
  wrix,
  beadsImage,
}:

let
  inherit (pkgs.lib) escapeShellArg makeBinPath mkForce;

  syncBranch = "team-beads";
  serviceImage = import ../../lib/services/image.nix {
    inherit pkgs;
    inherit (wrix.rustPackage) cacheServe;
  };
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
      yq-go
      wrix.rustPackage.wrix
    ]
  );
  commonEnvironment = ''
    export PATH=/run/wrappers/bin:/run/current-system/sw/bin:${testPath}
    export HOME=/home/alice
    export XDG_STATE_HOME="$HOME/.local/state"
    export XDG_CACHE_HOME="$HOME/.cache"
    export BD_NON_INTERACTIVE=1
    export BD_DISABLE_METRICS=1
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
    origin="$HOME/beads-origin.git"
    worktree="$repo/.git/beads-worktrees/${syncBranch}"
    git init --bare -q "$origin"
    git -C "$repo" init -q -b main
    git -C "$repo" config user.name "Wrix Test"
    git -C "$repo" config user.email "wrix@example.invalid"
    printf 'main\n' >"$repo/README.md"
    git -C "$repo" add README.md
    git -C "$repo" commit -qm "initial"
    git -C "$repo" switch -c "${syncBranch}" --quiet
    mkdir -p "$repo/.beads/dolt-remote"
    touch "$repo/.beads/dolt-remote/.keep"
    git -C "$repo" add .beads/dolt-remote/.keep
    git -C "$repo" commit -qm "beads initial"
    git -C "$repo" switch main --quiet
    git -C "$repo" worktree add "$worktree" "${syncBranch}" --quiet
    mkdir -p \
      "$repo/.beads" \
      "$repo/.wrix" \
      "$XDG_STATE_HOME" \
      "$XDG_CACHE_HOME"
    chmod 700 "$repo/.beads"
  '';
  initializeBeads = pkgs.writeShellScript "wrix-beads-system-initialize" ''
    set -euo pipefail
    ${commonEnvironment}

    repo="$HOME/beads-repo"
    remote="$repo/.git/beads-worktrees/${syncBranch}/.beads/dolt-remote"
    cd "$repo"
    data="$HOME/beads-init-data"
    socket="$repo/.wrix/dolt.sock"
    mkdir -p "$data"
    dolt sql-server --data-dir "$data" --host 127.0.0.1 --port 13307 \
      --socket "$socket" >"$HOME/beads-init.log" 2>&1 &
    server_pid=$!
    stop_bootstrap() {
      kill "$server_pid"
      wait "$server_pid" || [[ "$?" -eq 143 ]]
    }
    trap stop_bootstrap EXIT
    for _ in {1..200}; do
      [[ ! -S "$socket" ]] || break
      kill -0 "$server_pid"
      sleep 0.1
    done
    if [[ ! -S "$socket" ]]; then
      cat "$HOME/beads-init.log" >&2
      exit 1
    fi
    export BEADS_DOLT_SERVER_SOCKET="$socket"
    export BEADS_DOLT_AUTO_START=0

    bd init \
      --prefix wx \
      --skip-hooks \
      --skip-agents \
      --non-interactive \
      --server \
      --server-socket "$socket" \
      --database wx \
      >/dev/null
    stop_bootstrap
    trap - EXIT
    rmdir .beads/dolt
    mv "$data" .beads/dolt
    chmod 700 .beads
    if grep -q '^issue-prefix:' .beads/config.yaml; then
      sed -i 's/^issue-prefix:.*/issue-prefix: "wx"/' .beads/config.yaml
    else
      printf 'issue-prefix: "wx"\n' >>.beads/config.yaml
    fi
    if grep -q '^sync-branch:' .beads/config.yaml; then
      sed -i 's/^sync-branch:.*/sync-branch: "${syncBranch}"/' .beads/config.yaml
    else
      printf 'sync-branch: "%s"\n' '${syncBranch}' >>.beads/config.yaml
    fi
    if ! grep -q '^sync:' .beads/config.yaml; then
      printf 'sync:\n  mode: dolt-native\n' >>.beads/config.yaml
    fi
    wrix service start --no-cache >/dev/null
    wrix service dolt wait >/dev/null
    bd config set export.auto true >/dev/null
    bd dolt remote add origin "file://$remote" >/dev/null
    bd dolt commit >/dev/null
    bd dolt push >/dev/null
    rm -f .beads/issues.jsonl
    git remote add origin "$HOME/beads-origin.git"
    git push -u origin main "${syncBranch}" --quiet
  '';
  verifyCommandSurface = pkgs.writeShellScript "wrix-beads-system-command-surface" ''
    set -euo pipefail
    ${commonEnvironment}

    repo="$HOME/beads-repo"
    socket=$(cd "$repo" && wrix service dolt socket)
    export BEADS_DOLT_SERVER_SOCKET="$socket"
    export BEADS_DOLT_AUTO_START=0
    cd "$repo"

    verify_config() {
      local pattern="$1"
      if ! grep -E "$pattern" .beads/config.yaml >/dev/null; then
        printf 'missing Beads config pattern: %s\n' "$pattern" >&2
        cat .beads/config.yaml >&2
        exit 1
      fi
    }
    verify_config '^issue-prefix: "?wx"?$'
    verify_config '^sync-branch: "?${syncBranch}"?$'
    verify_config 'mode: dolt-native'
    yq -e '.export.auto == false or .["export.auto"] == false' .beads/config.yaml >/dev/null

    task_id=$(bd create --title "command task" --type task --priority=P2 --silent)
    bd show "$task_id" --json | jq -e \
      --arg id "$task_id" \
      '.[0].id == $id and (.[0].id | startswith("wx-")) and .[0].issue_type == "task" and .[0].priority == 2' \
      >/dev/null
    bd update "$task_id" --status=in_progress >/dev/null
    bd list --status=in_progress --json | jq -e \
      --arg id "$task_id" \
      'any(.[]; .id == $id)' \
      >/dev/null
    bd update "$task_id" --add-label=one --add-label=two --notes="command notes" >/dev/null
    bd update "$task_id" --remove-label=one >/dev/null
    bd show "$task_id" --json | jq -e \
      '.[0].notes == "command notes" and .[0].labels == ["two"]' \
      >/dev/null
    bd close "$task_id" >/dev/null

    blocker_id=$(bd create --title "command blocker" --type task --silent)
    dependent_id=$(bd create --title "command dependent" --type task --silent)
    bd dep add "$dependent_id" "$blocker_id" >/dev/null
    bd show "$dependent_id" --json | jq -e \
      --arg blocker "$blocker_id" \
      'any(.[0].dependencies[]; .id == $blocker and .dependency_type == "blocks")' \
      >/dev/null
    bd ready --json | jq -e \
      --arg blocker "$blocker_id" \
      --arg dependent "$dependent_id" \
      'any(.[]; .id == $blocker) and all(.[]; .id != $dependent)' \
      >/dev/null
    bd close "$blocker_id" >/dev/null
    bd ready --json | jq -e --arg id "$dependent_id" 'any(.[]; .id == $id)' >/dev/null

    for issue_type in bug feature epic chore decision; do
      issue_id=$(bd create --title "type $issue_type" --type "$issue_type" --silent)
      bd show "$issue_id" --json | jq -e \
        --arg issue_type "$issue_type" \
        '.[0].issue_type == $issue_type' \
        >/dev/null
    done
    for priority in 0 1 2 3 4 P0 P1 P2 P3 P4; do
      issue_id=$(bd create --title "priority $priority" --type task --priority="$priority" --silent)
      expected="''${priority#P}"
      bd show "$issue_id" --json | jq -e \
        --argjson expected "$expected" \
        '.[0].priority == $expected' \
        >/dev/null
    done
    if [[ -e .beads/issues.jsonl ]]; then
      printf 'a direct bd command recreated issues.jsonl after auto-export suppression\n' >&2
      exit 1
    fi
  '';
  verifyAutoExport = pkgs.writeShellScript "wrix-beads-system-auto-export" ''
    set -euo pipefail
    ${commonEnvironment}

    repo="$HOME/beads-repo"
    socket=$(cd "$repo" && wrix service dolt socket)
    export BEADS_DOLT_SERVER_SOCKET="$socket"
    export BEADS_DOLT_AUTO_START=0
    cd "$repo"

    for attempt in 1 2; do
      if ! wrix beads push >"$HOME/beads-auto-export-$attempt.out" \
        2>"$HOME/beads-auto-export-$attempt.err"; then
        cat "$HOME/beads-auto-export-$attempt.out" >&2
        cat "$HOME/beads-auto-export-$attempt.err" >&2
        exit 1
      fi
      if grep -F 'Warning: auto-export: git add failed' "$HOME/beads-auto-export-$attempt.err"; then
        printf 'auto-export warning remained enabled\n' >&2
        exit 1
      fi
      if [[ "$attempt" -eq 1 ]]; then
        sha256sum .beads/config.yaml >"$HOME/beads-config.sha256"
      else
        sha256sum --check "$HOME/beads-config.sha256"
      fi
    done
    if [[ "$(bd config get export.auto)" != "false" ]]; then
      printf 'auto-export config was not disabled\n' >&2
      exit 1
    fi
    yq -e '.export.auto == false or .["export.auto"] == false' .beads/config.yaml >/dev/null
    if [[ -e .beads/issues.jsonl ]]; then
      printf 'auto-export created issues.jsonl\n' >&2
      exit 1
    fi
  '';
  prepareSync = pkgs.writeShellScript "wrix-beads-system-prepare-sync" ''
    set -euo pipefail
    ${commonEnvironment}

    repo="$HOME/beads-repo"
    remote="$repo/.git/beads-worktrees/${syncBranch}/.beads/dolt-remote"
    socket=$(cd "$repo" && wrix service dolt socket)
    export BEADS_DOLT_SERVER_SOCKET="$socket"
    export BEADS_DOLT_AUTO_START=0
    cd "$repo"

    remote_digest() {
      find "$remote" -type f -printf '%P:%s\n' | sort | sha256sum | cut -d' ' -f1
    }

    before=$(remote_digest)
    issue_id=$(bd create --title "sandbox sync probe" --type task --silent)
    printf '%s\n' "$issue_id" >"$HOME/beads-issue-id"
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
      '{workspace:$workspace,env:[["BD_DISABLE_METRICS","1"]],agent_args:["bash","-euo","pipefail","-c",$command],mounts:[]}' \
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
        remote="$repo/.git/beads-worktrees/${syncBranch}/.beads/dolt-remote"
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

        bd sql "CALL DOLT_REMOTE('remove', 'origin')" >/dev/null
        bd sql "CALL DOLT_REMOTE('add', 'origin', 'file://$remote')" >/dev/null
        issue_id=$(<"$HOME/beads-issue-id")
        bd close "$issue_id" >/dev/null
        bd dolt commit >/dev/null

        real_bd=$(command -v bd)
        fallback_bin="$HOME/beads-fallback-bin"
        fallback_log="$HOME/beads-fallback.log"
        mkdir -p "$fallback_bin"
        cat >"$fallback_bin/bd" <<'BD_WRAPPER'
    #!/usr/bin/env bash
    set -euo pipefail

    printf '%s\n' "$*" >>"$WRIX_FALLBACK_LOG"
    if [[ "$1" == "dolt" && "$2" == "push" ]]; then
      "$WRIX_REAL_BD" sql "CALL DOLT_FETCH('origin')" >/dev/null
      printf 'non-fast-forward update rejected\n' >&2
      exit 1
    fi
    if [[ "$1" == "dolt" && "$2" == "pull" ]]; then
      "$WRIX_REAL_BD" sql \
        "UPDATE issues SET status='blocked' WHERE id='$WRIX_INTENT_ID'" \
        >/dev/null
      exit 0
    fi
    exec "$WRIX_REAL_BD" "$@"
    BD_WRAPPER
        chmod +x "$fallback_bin/bd"
        export WRIX_REAL_BD="$real_bd"
        export WRIX_FALLBACK_LOG="$fallback_log"
        export WRIX_INTENT_ID="$issue_id"
        export PATH="$fallback_bin:$PATH"

        set +e
        "$fallback_bin/bd" dolt push >"$HOME/wrapper-push.out" 2>"$HOME/wrapper-push.err"
        wrapper_push_status=$?
        set -e
        [[ "$wrapper_push_status" -eq 1 ]]
        grep -F 'non-fast-forward update rejected' "$HOME/wrapper-push.err" >/dev/null
        "$fallback_bin/bd" dolt pull
        "$real_bd" sql --csv "SELECT status FROM issues WHERE id='$issue_id'" \
          | grep -Fx 'blocked' >/dev/null
        "$real_bd" sql "UPDATE issues SET status='closed' WHERE id='$issue_id'" >/dev/null
        "$real_bd" dolt commit >/dev/null
        : >"$fallback_log"

        set +e
        wrix beads push >"$HOME/beads-fallback.out" 2>"$HOME/beads-fallback.err"
        fallback_status=$?
        set -e
        if [[ "$fallback_status" -eq 0 ]]; then
          printf 'conflicting real Dolt fallback unexpectedly succeeded\n' >&2
          exit 1
        fi
        grep -F 'dolt_commit_diff_issues' "$fallback_log" >/dev/null
        grep -F 'dolt_commit_diff_labels' "$fallback_log" >/dev/null
        grep -F 'pull-fallback diverged from local status/label intent' \
          "$HOME/beads-fallback.err" >/dev/null
        grep -F "$issue_id" "$HOME/beads-fallback.err" >/dev/null
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
      # VM shares need ordinary file I/O, not file handles or device-node creation.
      host.pkgs = mkForce (
        pkgs
        // {
          virtiofsd = pkgs.writeShellScriptBin "virtiofsd" ''
            set -euo pipefail
            exec ${pkgs.virtiofsd}/bin/virtiofsd \
              --inode-file-handles=never --modcaps=-mknod "$@"
          '';
        }
      );
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

    machine.succeed(as_alice("${initializeBeads}"), timeout=300)

    with subtest("wrix beads push disables real bd auto-export idempotently"):
        machine.succeed(as_alice("${verifyAutoExport}"), timeout=120)

    with subtest("real bd command and configuration surface"):
        machine.succeed(as_alice("${verifyCommandSurface}"), timeout=120)

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
