#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/lib/live-sandbox.sh
source "$SCRIPT_DIR/../lib/live-sandbox.sh"

TEST_TMP="$(mktemp -d -t wrix-live-profile.XXXXXX)"
trap 'rm -rf "$TEST_TMP"' EXIT
mkdir "$TEST_TMP/bin"
# shellcheck disable=SC2016 # The external uname fixture reads TEST_HOST when invoked.
printf '#!/usr/bin/env bash\nset -euo pipefail\nprintf "%%s\\n" "$TEST_HOST"\n' >"$TEST_TMP/bin/uname"
chmod +x "$TEST_TMP/bin/uname"
export PATH="$TEST_TMP/bin:$PATH"

for pair in 'Linux x86_64:x86_64-linux' 'Linux aarch64:aarch64-linux' 'Darwin arm64:aarch64-darwin'; do
  export TEST_HOST="${pair%:*}"
  wrix_write_profile_config "$TEST_TMP/profile.json" wrix-test:latest /unused direct '[]' docker-archive
  jq -e --arg system "${pair#*:}" '.schema == 1 and .system == $system and .agent.kind == "direct"' "$TEST_TMP/profile.json" >/dev/null
done

export TEST_HOST='Darwin x86_64'
if wrix_write_profile_config "$TEST_TMP/unsupported.json" wrix-test:latest /unused direct '[]' docker-archive 2>"$TEST_TMP/error"; then
  printf 'unsupported host was accepted\n' >&2
  exit 1
else
  [[ "$?" == 64 ]]
fi
[[ ! -e "$TEST_TMP/unsupported.json" ]]
grep -q 'unsupported live sandbox host: Darwin x86_64' "$TEST_TMP/error"
printf 'PASS: live ProfileConfig host systems\n'
