#!/usr/bin/env bash
# Shared Dolt SQL server lifecycle for test suites.
#
# Provides setup/teardown functions that start a single dolt sql-server
# shared across all test cases. Eliminates embedded dolt lock contention
# when multiple bd calls run concurrently (e.g., gc integration tests).
#
# Usage:
#   source tests/lib/dolt-server.sh
#   setup_shared_dolt_server [bind-host]   # default: 127.0.0.1
#   trap teardown_shared_dolt_server EXIT
#
# Exports: SHARED_DOLT_HOST, SHARED_DOLT_DIR, SHARED_DOLT_PORT, SHARED_DOLT_PID

# bd must never auto-start its own dolt sql-server or prompt on stdin —
# both leak orphan processes when tests run non-interactively.
export BEADS_DOLT_AUTO_START=0
export BD_NON_INTERACTIVE=1

# Kill orphaned dolt sql-server processes left over from previous test runs.
# Scans harness pidfiles (/tmp/*-test-dolt-*/) and sweeps bd-auto-started dolts
# (--loglevel=warning, no --data-dir) — but only when no live runner exists.
kill_stale_test_dolt_servers() {
  local killed=0
  for pidfile in /tmp/*-test-dolt-*/runner.pid; do
    [ -f "$pidfile" ] || continue

    local runner_pid dolt_pid dir
    runner_pid=$(cat "$pidfile" 2>/dev/null || echo "")
    [ -n "$runner_pid" ] || continue

    # If the runner is still alive, this is an active test run — skip
    if kill -0 "$runner_pid" 2>/dev/null; then
      continue
    fi

    # Runner is dead — kill the dolt server and clean up
    dir=$(dirname "$pidfile")
    dolt_pid=$(cat "$dir/dolt.pid" 2>/dev/null || echo "")
    if [ -n "$dolt_pid" ]; then
      kill "$dolt_pid" 2>/dev/null && killed=$((killed + 1)) || true
    fi
    rm -rf "$dir"
  done

  # Skip the bd-orphan sweep if any live harness runner exists.
  local live_runners=0
  for pidfile in /tmp/*-test-dolt-*/runner.pid; do
    [ -f "$pidfile" ] || continue
    local rpid
    rpid=$(cat "$pidfile" 2>/dev/null || echo "")
    if [ -n "$rpid" ] && kill -0 "$rpid" 2>/dev/null; then
      live_runners=$((live_runners + 1))
    fi
  done
  if [ "$live_runners" -eq 0 ]; then
    local bd_pid
    while read -r bd_pid; do
      [ -n "$bd_pid" ] || continue
      kill "$bd_pid" 2>/dev/null && killed=$((killed + 1)) || true
    done < <(pgrep -af 'dolt sql-server' \
              | grep -- '--loglevel=warning' \
              | grep -v -- '--data-dir' \
              | grep -v '/workspace/.wrapix' \
              | awk '{print $1}')
  fi

  if [ "$killed" -gt 0 ]; then
    echo "Cleaned up $killed orphaned dolt sql-server process(es) from previous run"
    sleep 0.2
  fi
}

# Start a single shared Dolt sql-server.
# Each caller gets its own database via a unique --prefix in bd init.
# Usage: setup_shared_dolt_server [bind-host]
# Exports: SHARED_DOLT_HOST, SHARED_DOLT_DIR, SHARED_DOLT_PORT, SHARED_DOLT_PID
setup_shared_dolt_server() {
  local bind_host="${1:-127.0.0.1}"

  # Clean up stale dolt processes from interrupted previous runs
  kill_stale_test_dolt_servers

  SHARED_DOLT_HOST="$bind_host"
  SHARED_DOLT_DIR=$(mktemp -d -t "wrapix-test-dolt-XXXXXX")

  # Initialize dolt data directory
  mkdir -p "$SHARED_DOLT_DIR/data"
  (cd "$SHARED_DOLT_DIR/data" && dolt init >/dev/null 2>&1)

  # Try up to 5 random ports to avoid collisions with existing services
  local attempts=5
  while [ $attempts -gt 0 ]; do
    SHARED_DOLT_PORT=$((20000 + RANDOM % 40000))

    # Skip port if already in use
    if bash -c "echo > /dev/tcp/${bind_host}/$SHARED_DOLT_PORT" 2>/dev/null; then
      attempts=$((attempts - 1))
      continue
    fi

    # Start server on this port
    dolt sql-server -H "$bind_host" -P "$SHARED_DOLT_PORT" \
      --data-dir="$SHARED_DOLT_DIR/data" \
      &>"$SHARED_DOLT_DIR/server.log" &
    SHARED_DOLT_PID=$!

    # Wait for server readiness, checking PID is still alive
    local retries=50
    local started=false
    while [ $retries -gt 0 ]; do
      # If dolt exited (e.g. port race), stop polling
      if ! kill -0 "$SHARED_DOLT_PID" 2>/dev/null; then
        break
      fi
      if bash -c "echo > /dev/tcp/${bind_host}/$SHARED_DOLT_PORT" 2>/dev/null; then
        started=true
        break
      fi
      sleep 0.1
      retries=$((retries - 1))
    done

    if [ "$started" = true ]; then
      break
    fi

    # Server failed on this port — clean up and retry
    kill "$SHARED_DOLT_PID" 2>/dev/null || true
    wait "$SHARED_DOLT_PID" 2>/dev/null || true
    attempts=$((attempts - 1))
  done

  if [ $attempts -eq 0 ]; then
    echo "ERROR: Shared dolt server failed to start after 5 port attempts" >&2
    cat "$SHARED_DOLT_DIR/server.log" >&2
    exit 1
  fi

  # Write PID files for stale process detection by concurrent/future runs
  echo "$$" > "$SHARED_DOLT_DIR/runner.pid"
  echo "$SHARED_DOLT_PID" > "$SHARED_DOLT_DIR/dolt.pid"

  export SHARED_DOLT_HOST SHARED_DOLT_DIR SHARED_DOLT_PORT SHARED_DOLT_PID
}

# Stop the shared Dolt server and clean up.
# Usage: teardown_shared_dolt_server (call once after all tests, typically via EXIT trap)
teardown_shared_dolt_server() {
  if [ -n "${SHARED_DOLT_PID:-}" ]; then
    kill "$SHARED_DOLT_PID" 2>/dev/null || true
    wait "$SHARED_DOLT_PID" 2>/dev/null || true
  fi

  if [ -n "${SHARED_DOLT_DIR:-}" ] && [ -d "$SHARED_DOLT_DIR" ]; then
    rm -rf "$SHARED_DOLT_DIR"
  fi
  unset SHARED_DOLT_HOST SHARED_DOLT_DIR SHARED_DOLT_PORT SHARED_DOLT_PID
}
