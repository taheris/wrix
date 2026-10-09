#!/usr/bin/env bash
set -euo pipefail

# shellcheck source=tests/sandbox/entrypoint-contract.sh
source "${REPO_ROOT:?}/tests/sandbox/entrypoint-contract.sh"

root="${PI_TEST_ROOT:?}"
platform="${PI_TEST_PLATFORM:?}"
agent="${PI_TEST_AGENT:-pi}"
workspace="$root/workspace"
home_dir="$root/home"
etc_wrix="$root/etc/wrix"
tool_dir="$root/tools"
entrypoint="$root/entrypoint.sh"
mkdir -p "$workspace/.claude" "$home_dir/.pi/agent"
write_fake_runtime_tools "$tool_dir"
prepare_wrix_etc "$etc_wrix" "$agent"
cp "${PI_TEST_AVAILABLE:?}" "$etc_wrix/mcp-available.json"
if [[ "$agent" == pi ]]; then
  rm "$tool_dir/pi"
  cp "${PI_TEST_SETTINGS:?}" "$etc_wrix/pi-agent/settings.json"
  cp "${PI_TEST_MODELS:?}" "$etc_wrix/pi-agent/models.json"
fi
rewrite_entrypoint "$platform" "$workspace" "$etc_wrix" "$entrypoint" "$home_dir"
HOST_UID="$(id -u)"
export HOME="$home_dir" HOST_UID PATH="$tool_dir:$PATH" WRIX_AGENT="$agent" WRIX_STDIO=1
bash "$entrypoint" "$@"
