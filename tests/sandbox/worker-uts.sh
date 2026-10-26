#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
work=$(mktemp -d -t wrix-worker-uts.XXXXXX)
trap 'rm -rf "$work"' EXIT
[[ "$(podman info --format '{{.Host.Security.Rootless}}')" == true ]]
profile=$(nix build "$root#test-image-worker-uts.profileConfig" --no-link --print-out-paths)
# This worker-only fixture does not need cache, Beads, or credential services.
jq '.services.nix_cache.enable = false' "$profile" > "$work/profile.json"
mkdir "$work/workspace"
host_name=$(uname -n)
jq -n --arg workspace "$work/workspace" --arg uts "$(readlink /proc/self/ns/uts)" \
  '{workspace: $workspace, env: [["TEST_HOST_UTS", $uts]], agent_args: [], git: {deploy: false, sign: false}, tool_caches: {shared: false}}' > "$work/spawn.json"
"${WRIX_TEST_WRIX_BIN:-wrix}" --profile-config "$work/profile.json" spawn --spawn-config "$work/spawn.json"
[[ "$(uname -n)" == "$host_name" ]]
