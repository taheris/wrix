#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
CONTAINER_UTIL="${CONTAINER_UTIL:-$REPO_ROOT/lib/util/container.sh}"
TEST_TMP="$(mktemp -d)"
trap 'rm -rf "$TEST_TMP"' EXIT

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  exit 1
}

write_fake_container() {
  local path="$1"

  cat >"$path" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 2 || "$1" != "inspect" ]]; then
  printf 'fake container: expected inspect <name>\n' >&2
  exit 64
fi

case "$2" in
  live) state="running" ;;
  stopped) state="stopped" ;;
  missing)
    printf '[]\n'
    exit 0
    ;;
  *)
    printf 'fake container: unknown container %s\n' "$2" >&2
    exit 1
    ;;
esac

cat <<JSON
[
  {
    "configuration" : {
      "id" : "$2"
    },
    "status" : {
      "state" : "$state"
    }
  }
]
JSON
FAKE
  chmod +x "$path"
}

test_fake_inspect_matches_apple_status_shape() {
  local inspect_output
  inspect_output="$(container inspect live)"

  if ! printf '%s\n' "$inspect_output" | jq -e '
    type == "array" and
    length == 1 and
    .[0].configuration.id == "live" and
    (.[0].status | type) == "object" and
    .[0].status.state == "running"
  ' >/dev/null; then
    fail "fake inspect output does not match the Apple container status shape"
  fi
  [[ "$inspect_output" == *$'\n'* ]] \
    || fail "fake inspect output is not pretty-printed like Apple container"
}

test_cr_is_running_parses_nested_apple_status() {
  cr_is_running live || fail "cr_is_running did not recognize the nested running state"
  if cr_is_running stopped; then
    fail "cr_is_running reported a nested stopped state as running"
  fi
}

test_cr_status_parses_nested_apple_status() {
  local status
  status="$(cr_status live)"
  [[ "$status" == "running" ]] \
    || fail "cr_status returned '$status' for the nested running state"
}

run_one() {
  local test_name="$1"
  "$test_name"
  printf 'PASS: %s\n' "$test_name"
}

main() {
  if [[ "$#" -gt 0 ]]; then
    run_one "$1"
    return 0
  fi

  run_one test_fake_inspect_matches_apple_status_shape
  run_one test_cr_is_running_parses_nested_apple_status
  run_one test_cr_status_parses_nested_apple_status
}

mkdir -p "$TEST_TMP/bin"
write_fake_container "$TEST_TMP/bin/container"
export PATH="$TEST_TMP/bin:$PATH"
export CR="container"
# shellcheck source=../../lib/util/container.sh
source "$CONTAINER_UTIL"
main "$@"
