# Ralph standalone container integration test.
#
# Exercises `mkRalph` without gas-city to verify a consumer that just wants
# ralph (no city orchestration) still gets the beads-dolt container bootstrap
# and the unix-socket-based bd connection.
#
# Scenarios:
#   shellhook-bootstraps-beads:
#     mkRalph.shellHook must start the per-workspace beads-dolt container,
#     export BEADS_DOLT_SERVER_SOCKET (+ AUTO_START=0), and create the
#     .wrapix/dolt.sock socket on the shared filesystem.
#   app-bootstraps-beads:
#     mkRalph.app.program must bootstrap beads-dolt before launching the
#     sandbox, so `nix run .#ralph` outside a devShell doesn't fall back to
#     bd's embedded autostart.
#   bd-reaches-socket-in-sandbox:
#     A sandbox launched via mkRalph can reach bd through the mounted
#     .wrapix/dolt.sock — the host's dolt container is the single source of
#     truth, matching the devShell behaviour.
#
# Requires podman. Run via: nix run .#test-ralph-container
{
  pkgs,
  system,
  linuxPkgs,
}:

let
  inherit (pkgs) lib writeShellScriptBin;

  sandbox = import ../../lib/sandbox { inherit pkgs system linuxPkgs; };
  beads = import ../../lib/beads { inherit pkgs linuxPkgs; };
  ralph = import ../../lib/ralph {
    inherit pkgs beads;
    inherit (sandbox) mkSandbox;
  };
  wrapixLib = import ../../lib { inherit pkgs system linuxPkgs; };

  testProfile = sandbox.profiles.base // {
    name = "ralph-test";
  };

  # Mock claude used only to prove the entrypoint's bd-via-socket path
  # resolves inside the sandbox. Writes an agent-check log to /workspace so
  # the host-side test can assert on it.
  mockClaude = pkgs.writeShellScriptBin "claude" ''
    set -euo pipefail

    LOG=/workspace/agent-check.log
    : > "$LOG"

    if [ -S /workspace/.wrapix/dolt.sock ]; then
      echo "AGENT_CHECK: socket PRESENT" >> "$LOG"
    else
      echo "AGENT_CHECK: socket MISSING" >> "$LOG"
    fi

    echo "AGENT_CHECK: socket_env=''${BEADS_DOLT_SERVER_SOCKET:-UNSET}" >> "$LOG"
    echo "AGENT_CHECK: auto_start=''${BEADS_DOLT_AUTO_START:-UNSET}" >> "$LOG"

    if bd list >/dev/null 2>&1; then
      echo "AGENT_CHECK: bd-list PRESENT" >> "$LOG"
    else
      echo "AGENT_CHECK: bd-list MISSING" >> "$LOG"
      bd list 2>> "$LOG" || true
    fi
  '';

  # Build the ralph instance under test. Its .sandbox.image is the live
  # image the test will load — same code path a real consumer hits.
  ralphInstance = ralph.mkRalph { profile = testProfile; };

  # Replace the real claude with mockClaude in the image so we can observe
  # what the sandbox sees without needing a real LLM.
  testImage = sandbox.mkImage {
    inherit (ralphInstance.sandbox) profile;
    entrypointSh = ../../lib/sandbox/linux/entrypoint.sh;
    claudePkg = mockClaude;
    asTarball = pkgs.stdenv.isDarwin;
  };

  inherit (pkgs.stdenv) isDarwin;

  # Live devShell PATH — matches the shell a standalone consumer enters. If
  # a tool needed by the shellHook or app is missing here, the test fails
  # the same way live would.
  liveDevShell = wrapixLib.mkDevShell {
    inherit (ralphInstance) shellHook;
    packages = ralphInstance.packages ++ [ pkgs.podman ];
  };

  systemDeps = [
    pkgs.git
    pkgs.jq
    pkgs.util-linux
  ];
  livePath = lib.makeBinPath (
    liveDevShell.nativeBuildInputs ++ pkgs.stdenv.initialPath ++ systemDeps
  );

  script = writeShellScriptBin "test-ralph-container" ''
    set -euo pipefail

    if ! command -v podman >/dev/null 2>&1; then
      echo "SKIP: podman not found — ralph container test requires podman on the host."
      exit 0
    fi

    export LIVE_PATH="${livePath}"
    export PATH="${livePath}"

    # Strip inherited BEADS_/BD_ env so the test only sees state it sets.
    # Without this, the outer devShell's variables point bd at the host
    # workspace's dolt container.
    for _v in ''${!BEADS_@} ''${!BD_@}; do unset "$_v"; done

    PASSED=0
    FAILED=0
    WS=""
    RUN_ID="$(head -c4 /dev/urandom | od -An -tx1 | tr -d ' \n')"

    cleanup() {
      echo ""
      echo "--- Cleanup ---"
      if [ -n "$WS" ]; then
        beads-dolt stop "$WS" 2>/dev/null || true
        rm -rf "$WS" 2>/dev/null || true
      fi
      echo ""
      echo "========================================"
      echo "PASSED: $PASSED  FAILED: $FAILED"
      if [ "$FAILED" -gt 0 ]; then
        echo "SOME TESTS FAILED"
        exit 1
      fi
      echo "ALL TESTS PASSED"
    }
    trap cleanup EXIT

    subtest() {
      local name="$1"
      shift
      echo ""
      echo "--- $name ---"
      if ( set -euo pipefail; "$@" ); then
        echo "PASS: $name"
        PASSED=$((PASSED + 1))
      else
        echo "FAIL: $name"
        FAILED=$((FAILED + 1))
      fi
    }

    echo "=== Ralph Standalone Container Integration Test (run: $RUN_ID) ==="

    # CONTAINER_HOST-aware workspace location: bind-mount paths must exist
    # on the host that runs podman.
    if [ -n "''${CONTAINER_HOST:-}" ]; then
      WS="$(mktemp -d /workspace/.wrapix/ralphtest-XXXXXX)"
      if [[ -z "''${GC_HOST_WORKSPACE:-}" ]]; then
        GC_HOST_WORKSPACE="$(awk '$5 == "/workspace" {print $4; exit}' /proc/self/mountinfo)"
      fi
      export GC_HOST_WORKSPACE="''${GC_HOST_WORKSPACE}''${WS#/workspace}"
      unset GC_HOST_BEADS
    else
      WS="$(mktemp -d -t ralphtest-XXXXXX)"
    fi
    WS="$(cd "$WS" && pwd -P)"
    export WS
    echo "  > workspace: $WS"
    cd "$WS"

    # Isolate dolt global config so the test doesn't touch ~/.dolt/.
    export HOME="$WS"

    setup_workspace() {
      git init -b main
      git config user.email test@test
      git config user.name test
      git config commit.gpgsign false
      dolt config --global --add user.email test@test
      dolt config --global --add user.name test
      git commit --allow-empty -m initial

      mkdir -p .wrapix .claude

      # bd init --server creates .beads/dolt so beads-dolt has something
      # to serve. Stop bd's embedded dolt — the test uses the container.
      bd init --prefix rt --skip-hooks --skip-agents --non-interactive --server
      bd dolt stop
      rm -f .beads/dolt-server.pid .beads/dolt-server.lock .beads/dolt-server.port

      # Create a tracked bead so `bd list` has something to return from
      # inside the sandbox — proves the socket path is live, not just present.
      bd create --title="ralph-test-bead" --type=task --priority=2 --non-interactive

      git add -A
      git commit -m "workspace setup"

      echo "  > workspace ready"
    }
    subtest "Set up workspace" setup_workspace

    # ================================================================
    # mkRalph.shellHook: starts beads-dolt + exports env vars
    # ================================================================

    verify_shellhook_bootstrap() {
      # Run the shellHook in a clean subshell with only LIVE_PATH on PATH,
      # then print the env it exported. Matches how a standalone consumer's
      # devShell would behave.
      local env_out
      env_out="$(
        env -i HOME="$HOME" PATH="$LIVE_PATH" PWD="$WS" \
          CONTAINER_HOST="''${CONTAINER_HOST:-}" \
          GC_HOST_WORKSPACE="''${GC_HOST_WORKSPACE:-}" \
          bash -c '
            set -euo pipefail
            cd "$PWD"
            ${ralphInstance.shellHook}
            echo "---ENV---"
            env | grep -E "^(BEADS_|WRAPIX_|RALPH_)" | sort
          '
      )"

      echo "$env_out"

      echo "$env_out" | grep -qE "^BEADS_DOLT_SERVER_SOCKET=.*/\.wrapix/dolt\.sock$" || {
        echo "FAIL: BEADS_DOLT_SERVER_SOCKET not set to .wrapix/dolt.sock"
        return 1
      }
      echo "$env_out" | grep -q "^BEADS_DOLT_AUTO_START=0$" || {
        echo "FAIL: BEADS_DOLT_AUTO_START not set to 0"
        return 1
      }
      if echo "$env_out" | grep -qE "^BEADS_DOLT_SERVER_(HOST|PORT)="; then
        echo "FAIL: legacy BEADS_DOLT_SERVER_HOST/PORT leaked into env"
        return 1
      fi
      echo "$env_out" | grep -q "^RALPH_TEMPLATE_DIR=" || {
        echo "FAIL: RALPH_TEMPLATE_DIR not exported by shellHook"
        return 1
      }
      echo "$env_out" | grep -q "^RALPH_METADATA_DIR=" || {
        echo "FAIL: RALPH_METADATA_DIR not exported by shellHook"
        return 1
      }
    }
    subtest "mkRalph.shellHook exports beads + ralph env" verify_shellhook_bootstrap

    verify_socket_created() {
      # beads-dolt start creates the socket asynchronously inside the
      # container. Give it a bounded window.
      for _i in $(seq 1 50); do
        [ -S "$WS/.wrapix/dolt.sock" ] && return 0
        sleep 0.2
      done
      echo "FAIL: .wrapix/dolt.sock not created after shellHook"
      beads-dolt status "$WS" | sed 's/^/  /' || true
      return 1
    }
    subtest "mkRalph.shellHook creates .wrapix/dolt.sock" verify_socket_created

    verify_container_running() {
      local name
      name="$(beads-dolt name "$WS")"
      local state
      state="$(podman inspect --format '{{.State.Status}}' "$name" 2>/dev/null || echo not-found)"
      if [ "$state" != "running" ]; then
        echo "FAIL: container $name is $state (expected running)"
        return 1
      fi
    }
    subtest "mkRalph.shellHook starts beads-dolt container" verify_container_running

    # ================================================================
    # mkRalph.app: bootstraps beads-dolt before launching ralph
    # ================================================================
    #
    # The app is a wrapper around `ralph <args>`. We don't drive a full
    # ralph loop here — just verify the generated shell script contains
    # the beads bootstrap. Pairs with the shellHook test: both paths
    # must bootstrap identically, or `nix run .#ralph` outside a devShell
    # falls back to bd's embedded autostart.

    verify_app_has_bootstrap() {
      local prog="${ralphInstance.app.program}"
      # app.program is a writeShellScriptBin wrapper; its text lives in
      # the store with the beads-dolt start call + env exports.
      if ! grep -q "beads-dolt start" "$prog"; then
        echo "FAIL: ralph app program does not bootstrap beads-dolt"
        return 1
      fi
      if ! grep -q "BEADS_DOLT_AUTO_START=0" "$prog"; then
        echo "FAIL: ralph app program does not export BEADS_DOLT_AUTO_START=0"
        return 1
      fi
    }
    subtest "mkRalph.app bootstraps beads-dolt before launch" verify_app_has_bootstrap

    # ================================================================
    # bd reaches dolt via socket from inside sandbox launched by mkRalph
    # ================================================================

    verify_sandbox_image_loads() {
      ${if isDarwin then "cat ${testImage}" else "${testImage}"} | podman load -q >/dev/null
    }
    subtest "Load mock-claude sandbox image" verify_sandbox_image_loads

    # Derive the image name the way lib/sandbox/image.nix does. We can't
    # easily introspect testImage's tag from the shell — use the loaded tag.
    IMAGE_REF=""
    resolve_image_ref() {
      IMAGE_REF="$(podman images --format '{{.Repository}}:{{.Tag}}' \
        | grep "wrapix-${testProfile.name}" \
        | head -1)"
      [ -n "$IMAGE_REF" ] || { echo "FAIL: could not find loaded image"; return 1; }
    }
    subtest "Resolve sandbox image reference" resolve_image_ref

    verify_bd_via_socket() {
      # Run the sandbox image directly with the beads socket bind-mounted
      # and the mock claude as the entrypoint. Matches what a ralph
      # consumer does: start beads-dolt on host, then launch the sandbox.
      rm -f "$WS/agent-check.log"

      local host_ws="''${GC_HOST_WORKSPACE:-$WS}"
      podman run --rm \
        --network host \
        --userns=keep-id \
        -v "$host_ws:/workspace:rw" \
        -e WRAPIX_NETWORK=open \
        "$IMAGE_REF" /bin/bash -c 'cd /workspace && claude' \
        || {
          echo "FAIL: sandbox run failed"
          [ -f "$WS/agent-check.log" ] && cat "$WS/agent-check.log" | sed 's/^/  /'
          return 1
        }

      local log="$WS/agent-check.log"
      [ -f "$log" ] || { echo "FAIL: agent-check log not produced"; return 1; }

      cat "$log" | sed 's/^/  /'

      grep -q "AGENT_CHECK: socket PRESENT" "$log" || {
        echo "FAIL: sandbox cannot see .wrapix/dolt.sock"
        return 1
      }
      grep -q "AGENT_CHECK: socket_env=/workspace/.wrapix/dolt.sock" "$log" || {
        echo "FAIL: BEADS_DOLT_SERVER_SOCKET not exported to sandbox"
        return 1
      }
      grep -q "AGENT_CHECK: auto_start=0" "$log" || {
        echo "FAIL: BEADS_DOLT_AUTO_START not set inside sandbox"
        return 1
      }
      grep -q "AGENT_CHECK: bd-list PRESENT" "$log" || {
        echo "FAIL: bd list failed via socket inside sandbox"
        return 1
      }
    }
    subtest "bd reaches dolt via socket from sandbox" verify_bd_via_socket
  '';

in
{
  inherit script testImage;
  imageName = "localhost/wrapix-${testProfile.name}";
}
