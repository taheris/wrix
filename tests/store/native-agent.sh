#!/usr/bin/env bash
set -euo pipefail
set -E
umask 077
workflow_phase=admission
workflow_failed() {
  local status="$?"
  printf 'Native installed workflow failed in %s (status %s).\n' "$workflow_phase" "$status" >&2
  exit "$status"
}
trap workflow_failed ERR
name="$1"
public=false
case "$name" in
  public-first|public-second|public-replacement|public-peer|public-invalid) public=true; name="${name#public-}" ;;
esac
[[ "$name" == first || "$name" == second || "$name" == replacement || "$name" == peer || "$name" == fault || ( "$public" == true && "$name" == invalid ) ]]
[[ -d /control && -d /workspace/.git || -f /workspace/.git ]]
workflow_phase=client-configuration
[[ -f /run/wrix/nix-pool/ssh_config && -r /run/wrix/nix-pool/key ]]
workflow_phase=private-state
[[ ! -e /nix/var/nix/db/db.sqlite && ! -e /var/lib/wrix/nix-pool ]]
if [[ "$public" == true && "$name" == invalid ]]; then
  # Reach the real admitted worker and live endpoint before the injected agent
  # failure; an earlier provisioning error cannot satisfy launch-failure coverage.
  nix store info --json > /control/invalid-agent-store-info.json
  jq -e '.trusted == false' /control/invalid-agent-store-info.json
  printf '{"intentional_exit":23}\n' > /control/invalid-agent-reached.json
  exit 23
fi
events="/control/$name-nix-events.log"
: > "$events"
if [[ "$name" == fault ]]; then
  workflow_phase=fault-probe
  nix store info --json | jq -e '.trusted == false'
  fault_path=$(nix build --impure --file "$WRIX_NATIVE_WORKFLOW" --out-link /workspace/fault-result --print-out-paths)
  [[ "$("$fault_path/bin/probe")" == installed ]]
  # After fail-stop the real shared projection is unavailable, including
  # sleep and nix. Wait using the already-running shell, with no executable
  # copy or private-store fallback. This FIFO's open writer prevents EOF.
  mkfifo /tmp/wrix-native-fault-wait
  exec {fault_wait_fd}<> /tmp/wrix-native-fault-wait
  printf '%s' "$fault_path" > /control/fault-path
  printf '{"native_client_live":true}\n' > /control/fault-ready.json
  while [[ ! -f /control/fault-recheck ]]; do
    read -r -t 0.05 -u "$fault_wait_fd" || :
  done
  if nix store info --json > /control/fault-disconnect.log 2>&1; then
    printf 'Service endpoint survived unclean daemon death without fail-stop\n' >&2
    exit 74
  else
    fault_disconnect_status=$?
  fi
  printf '{"native_disconnect_observed":true,"client_exit_status":%s}\n' "$fault_disconnect_status" > /control/fault-disconnected.json
  while [[ ! -f /control/fault-exit ]]; do
    read -r -t 0.05 -u "$fault_wait_fd" || :
  done
  exec {fault_wait_fd}>&-
  exit 0
fi
if [[ "$name" == peer ]]; then
  nix store info --json | jq -e '.trusted == false'
  peer_path=$(nix build --impure --file "$WRIX_NATIVE_PEER_WORKFLOW" --no-link --print-out-paths)
  [[ "$(< "$peer_path")" == peer-only ]]
  printf '%s' "$peer_path" > /control/peer-path
  printf '{"peer_live":true}\n' > /control/peer-ready.json
  while [[ ! -f /control/peer-exit ]]; do sleep 0.05; done
  [[ "$(< "$peer_path")" == peer-only ]]
  exit 0
fi
workflow_phase=evaluation
started=$(date +%s%N)
boot=$(< /proc/sys/kernel/random/boot_id)
machine=$(uname -m)
path=$(nix --log-format internal-json eval --impure --raw --file "$WRIX_NATIVE_WORKFLOW" --apply 'drv: drv.outPath' 2>> "$events")
if [[ "$name" == replacement ]]; then
  [[ -e "$path/bin/probe" && -e "$WRIX_NATIVE_COLD_PATH/bin/hello" ]]
  prior_miss=false
else
  [[ ! -e "$path" && ! -e "$WRIX_NATIVE_COLD_PATH" ]]
  prior_miss=true
fi
jq -n --arg system "$WRIX_NATIVE_SYSTEM" --arg machine "$machine" --arg boot_id "$boot" --arg path "$path" --argjson prior_miss "$prior_miss" '{system:$system,machine:$machine,boot_id:$boot_id,path:$path,prior_miss:$prior_miss}' > "/control/$name-ready.json.tmp"
mv "/control/$name-ready.json.tmp" "/control/$name-ready.json"
while [[ ! -f /control/go ]]; do sleep 0.05; done
workflow_phase=pool-isolation
peer_path=$(< /control/peer-path)
[[ ! -e "$peer_path" ]]
if nix path-info "$peer_path" > "/control/$name-peer-denied" 2>&1; then
  printf 'Worker endpoint read a known-live peer-pool path\n' >&2
  exit 73
fi
grep -Ei 'not valid|not in the Nix store|does not exist|cannot build missing' "/control/$name-peer-denied"
nix --log-format internal-json store info --json 2>> "$events" > "/control/$name-store-info.json"
jq -e '.trusted == false' "/control/$name-store-info.json"
workflow_phase=substitution
nix-store --log-format internal-json --realise "$WRIX_NATIVE_COLD_PATH" > "/control/$name-substituted-path" 2> "/control/$name-substitution-events.log"
[[ "$(< "/control/$name-substituted-path")" == "$WRIX_NATIVE_COLD_PATH" ]]
[[ "$("$WRIX_NATIVE_COLD_PATH/bin/hello")" == 'Hello, world!' ]]
workflow_phase=build
nix --log-format internal-json build --impure --file "$WRIX_NATIVE_WORKFLOW" --out-link "/workspace/$name-result" 2> "/control/$name-build-events.log"
[[ -e "$path/bin/probe" && "$(< "$path/build-uid")" != 0 ]]
workflow_phase=run
nix --log-format internal-json run --impure --file "$WRIX_NATIVE_WORKFLOW" 2>> "$events" > "/control/$name-run"
[[ "$(< "/control/$name-run")" == installed ]]
workflow_phase=develop
# The child shell consumes the installed development environment value.
# shellcheck disable=SC2016
nix --log-format internal-json develop --impure --override-flake nixpkgs "path:$WRIX_NATIVE_BASH_FLAKE" --file "$WRIX_NATIVE_WORKFLOW" --command bash -euc '[[ "$WRIX_POOL_TEST" == installed ]]' 2>> "$events"
workflow_phase=profile
mkdir -p "$HOME/.local/state/nix/profiles"
nix --log-format internal-json profile install --impure --file "$WRIX_NATIVE_WORKFLOW" --profile "$HOME/.local/state/nix/profiles/native-test" 2>> "$events"
[[ "$("$HOME/.local/state/nix/profiles/native-test/bin/probe")" == installed ]]
workflow_phase=checks
nix --log-format internal-json flake check --impure --no-write-lock-file "path:$WRIX_NATIVE_CHECKS" 2>> "$events"
workflow_phase=rust-workflow
project="/workspace/native-rust-$name"
mkdir "$project"
cp -R "$WRIX_NATIVE_PROJECT/." "$project/"
chmod -R u+w "$project"
(
  cd "$project"
  cargo build --locked --offline
  cargo clippy --locked --offline --all-targets -- -D warnings
  cargo nextest run --locked --offline
  [[ "$(./target/debug/native-installed-workflow)" == native-installed ]]
)
workflow_phase=untrusted-boundary
if touch /nix/store/native-worker-mutation; then
  printf 'Writable worker projection\n' >&2
  exit 65
fi
if nix-store --verify --check-contents --repair > "/control/$name-repair" 2>&1; then
  printf 'Untrusted worker obtained repair authority\n' >&2
  exit 66
fi
grep -E 'not privileged|not allowed|not trusted' "/control/$name-repair"
if nix-store --import < /control/unsigned.nar > "/control/$name-import" 2>&1; then
  printf 'Untrusted worker imported unsigned input-addressed data\n' >&2
  exit 67
fi
grep -F 'lacks a signature' "/control/$name-import"
nix build --impure --file "$WRIX_NATIVE_WORKFLOW" --no-link --option sandbox false --option build-users-group '' > "/control/$name-options" 2>&1
grep -Ei 'ignor.*(sandbox|build-users-group)|not trusted|restricted setting' "/control/$name-options"
[[ "$(< "$path/build-uid")" != 0 ]]
if ssh -F /run/wrix/nix-pool/ssh_config root@wrix-nix-pool control < /dev/null > "/control/$name-admin" 2>&1; then
  printf 'Worker obtained pool management authority\n' >&2
  exit 68
fi
grep -F 'Permission denied' "/control/$name-admin"
printf 'get %s/bin/probe /tmp/native-export-read\n' "$path" | sftp -F /run/wrix/nix-pool/ssh_config -b - nix-export@wrix-nix-pool
cmp /tmp/native-export-read "$path/bin/probe"
if printf 'put /tmp/native-export-read %s/bin/forbidden\n' "$path" | sftp -F /run/wrix/nix-pool/ssh_config -b - nix-export@wrix-nix-pool > "/control/$name-export-write" 2>&1; then
  printf 'Server filesystem export accepted writes\n' >&2
  exit 69
fi
grep -Ei 'denied|read.only|failure' "/control/$name-export-write"
outside=$(< /control/outside-path)
if cat "$outside"; then
  printf 'Worker read known-live host control\n' >&2
  exit 70
fi
if printf 'get %s /tmp/native-stolen\n' "$outside" | sftp -F /run/wrix/nix-pool/ssh_config -b - nix-export@wrix-nix-pool > "/control/$name-export-outside" 2>&1; then
  printf 'Server export escaped its jail\n' >&2
  exit 71
fi
grep -Ei 'denied|not found|no such file' "/control/$name-export-outside"
[[ ! -e /tmp/native-stolen ]]
[[ -e "$path/bin/probe" ]]
finished=$(date +%s%N)
python3 - "$BASHPID" > "/control/$name-io.json" <<'PY'
import json
import pathlib
import sys
fields = {}
for line in pathlib.Path('/proc/' + sys.argv[1] + '/io').read_text().splitlines():
    key, value = line.split(':', 1)
    fields[key] = int(value)
assert all(key in fields for key in ('rchar', 'wchar', 'read_bytes', 'write_bytes'))
print(json.dumps(fields))
PY
jq -n --arg path "$path" --rawfile run_stdout "/control/$name-run" --slurpfile io "/control/$name-io.json" --argjson elapsed_ns "$((finished - started))" '{path:$path,run_stdout:$run_stdout,io:$io[0],elapsed_ns:$elapsed_ns,build:true,develop:true,run:true,profile:true,cargo:true,clippy:true,nextest:true,check:true,readonly:true,untrusted:true,strict:true,signature_denied:true,repair_denied:true,options_denied:true,admin_denied:true,outside_denied:true,peer_denied:true,dynamic_visible:true}' > "/control/$name-done.json.tmp"
mv "/control/$name-done.json.tmp" "/control/$name-done.json"
if [[ "$name" == first ]]; then
  if [[ "$public" == false ]]; then
    while [[ ! -f /control/resources-go ]]; do sleep 0.05; done
  fi
  if [[ "$public" == false ]]; then
  printf '{}\n' > /control/resource-results.json
  for kind in public private ipv6 ordinary disk pids cpu; do
    workflow_phase="resource-$kind"
    succeeded=false
    if result=$(nix-build --no-out-link --option substituters '' --expr "(import $WRIX_NATIVE_RESOURCES) \"$kind\"" 2> "/control/resource-$kind.log"); then
      if [[ -f "$result" && "$(< "$result")" == ok ]]; then succeeded=true; fi
    fi
    jq --arg kind "$kind" --argjson succeeded "$succeeded" '. + {($kind): $succeeded}' /control/resource-results.json > /control/resource-results.json.tmp
    mv /control/resource-results.json.tmp /control/resource-results.json
  done
  workflow_phase=resource-memory
  oom_failed=true
  if nix-build --no-out-link --option substituters '' --expr "(import $WRIX_NATIVE_RESOURCES) \"memory\"" > /control/resource-memory.log 2>&1; then
    oom_failed=false
  fi
  nix store info --json | jq -e '.trusted == false'
  [[ "$("$path/bin/probe")" == installed ]]
  jq --argjson oom_failed "$oom_failed" '{public_fod:.public,private_fod_denied:.private,ipv6_fod_denied:.ipv6,ordinary_network_denied:.ordinary,disk_bound:.disk,pids_bound:.pids,cpu_work_completed:.cpu,oom_failed:$oom_failed,live_output_preserved:true}' /control/resource-results.json > /control/first-resources.json.tmp
  mv /control/first-resources.json.tmp /control/first-resources.json
  fi
  workflow_phase=controller-restart
  while [[ ! -f /control/recheck ]]; do sleep 0.05; done
  nix --log-format internal-json store info --json 2>> "$events" | jq -e '.trusted == false'
  nix --log-format internal-json build --impure --file "$WRIX_NATIVE_WORKFLOW" --out-link /workspace/after-controller-restart 2>> "$events"
  [[ "$("$path/bin/probe")" == installed ]]
  printf '{"native_client_survived":true}\n' > /control/first-resumed.json
fi
workflow_phase=retirement
while [[ ! -f "/control/$name-exit" ]]; do sleep 0.05; done
[[ "$("$HOME/.local/state/nix/profiles/native-test/bin/probe")" == installed ]]
