#!/usr/bin/env bash
set -euo pipefail
[[ "$#" == 1 ]]
entrypoint=$1

show_failure_logs() {
  local status=$? drv
  for drv in /nix/store/*-wrix-pool-*.drv; do
    if [[ -f "$drv" ]]; then
      if ! nix log "$drv"; then
        printf 'Native build log unavailable: %s\n' "$drv" >&2
      fi
    fi
  done
  return "$status"
}
trap show_failure_logs ERR

expect_refusal() {
  local expected=$1
  shift
  if "$@"; then
    printf 'Unexpected bootstrap admission.\n' >&2
    exit 1
  else
    [[ "$?" == "$expected" ]]
  fi
  [[ ! -e /var/lib/wrix/nix-pool/owner-active && ! -e /var/lib/wrix/nix-pool/nix ]]
}
expect_refusal 64 "$entrypoint" invalid
expect_refusal 64 setpriv --reuid=2000 --regid=2000 --clear-groups "$entrypoint"
[[ ! -e /var/lib/wrix/nix-pool/ownership.lock ]]
chmod 0755 /var/lib/wrix/nix-pool
expect_refusal 1 "$entrypoint"
[[ $(stat -c %a /var/lib/wrix/nix-pool) == 755 && ! -e /var/lib/wrix/nix-pool/ownership.lock ]]
chmod 0700 /var/lib/wrix/nix-pool
printf 'alias control\n' > /tmp/lock-target
chmod 0600 /tmp/lock-target
ln -s /tmp/lock-target /var/lib/wrix/nix-pool/ownership.lock
expect_refusal 1 "$entrypoint"
[[ $(</tmp/lock-target) == 'alias control' ]]
rm /var/lib/wrix/nix-pool/ownership.lock
printf 'unknown retained payload\n' > /var/lib/wrix/nix-pool/unknown
expect_refusal 73 "$entrypoint"
[[ $(</var/lib/wrix/nix-pool/unknown) == 'unknown retained payload' ]]
rm /var/lib/wrix/nix-pool/unknown

printf 'world-readable control\n' > /var/empty/visible-outside-build
chmod 0644 /var/empty/visible-outside-build
bash_store=$(dirname "$(dirname "$(readlink -f "$(command -v bash)")")")
core_store=$(dirname "$(dirname "$(readlink -f "$(command -v coreutils)")")")
setpriv --reuid=30001 --regid=30000 --clear-groups "$bash_store/bin/bash" \
  -c '[[ -r /var/empty/visible-outside-build ]]'
export NIX_CONFIG=$'sandbox = false\nsandbox-fallback = true\nbuild-users-group =\n'
"$entrypoint" > /tmp/pool-bootstrap.log 2>&1 &
bootstrap_pid=$!
unset NIX_CONFIG
export NIX_CONF_DIR=/etc/nix NIX_REMOTE=daemon
ready=false
for _attempt in {1..200}; do
  if [[ -f /run/wrix-nix-pool/daemon.pid ]] && nix store info; then
    ready=true
    break
  fi
  if ! kill -0 "$bootstrap_pid"; then
    cat /tmp/pool-bootstrap.log
    exit 1
  fi
  sleep 0.1
done
if [[ "$ready" != true ]]; then
  cat /tmp/pool-bootstrap.log
  exit 1
fi
config=$'\n'"$(nix config show)"$'\n'
for expected in 'sandbox = true' 'sandbox-fallback = false' 'build-users-group = nixbld' \
  'max-jobs = 1' 'cores = 2' 'require-sigs = true'; do
  [[ "$config" == *$'\n'"$expected"$'\n'* ]]
done
architecture=$(uname -m)
case "$architecture" in
  x86_64 | aarch64) export PROBE_SYSTEM="$architecture-linux" ;;
  *) printf 'Unsupported guest architecture: %s\n' "$architecture" >&2; exit 1 ;;
esac
export PROBE_BASH_STORE="$bash_store" PROBE_CORE_STORE="$core_store"
info=$(setpriv --reuid=2000 --regid=2000 --clear-groups nix store info 2>&1)
[[ "$info" == *'Trusted: 0'* ]]
output=$(setpriv --reuid=2000 --regid=2000 --clear-groups \
  nix-build --no-out-link --option substituters '' --option sandbox false \
  --option build-users-group '' --expr '
  let
    bash = builtins.storePath (builtins.getEnv "PROBE_BASH_STORE");
    core = builtins.storePath (builtins.getEnv "PROBE_CORE_STORE");
  in builtins.derivation {
    name = "wrix-pool-bootstrap-strict";
    system = builtins.getEnv "PROBE_SYSTEM";
    builder = bash + "/bin/bash";
    args = [ "-c" ("[[ ! -e /var/empty/visible-outside-build ]] && " + core + "/bin/id -u > $out") ];
  }
')
[[ $(<"$output") != 0 ]]
exec 3<>/dev/tcp/127.0.0.1/18443
IFS= read -r control <&3
exec 3<&- 3>&-
[[ "$control" == ok ]]
exec 3<>/dev/tcp/::1/18443
IFS= read -r control <&3
exec 3<&- 3>&-
[[ "$control" == ok ]]
for network_case in public private ipv6 ordinary; do
  export PROBE_NETWORK_CASE="$network_case"
  network_output=$(setpriv --reuid=2000 --regid=2000 --clear-groups \
    nix-build --no-out-link --option substituters '' --expr '
    let
      bash = builtins.storePath (builtins.getEnv "PROBE_BASH_STORE");
      kind = builtins.getEnv "PROBE_NETWORK_CASE";
      target = if kind == "public" then "93.184.216.34" else if kind == "ipv6" then "::1" else "127.0.0.1";
      command = if kind == "public"
        then "exec 3<>/dev/tcp/" + target + "/18443; IFS= read -r value <&3; [[ $value == ok ]]; printf ok > $out"
        else "if (exec 3<>/dev/tcp/" + target + "/18443) 2>denial; then exit 1; fi; [[ $(<denial) == *Operation?not?permitted* ]]; printf ok > $out";
      base = {
        name = "wrix-pool-egress-" + kind;
        system = builtins.getEnv "PROBE_SYSTEM";
        builder = bash + "/bin/bash";
        LC_ALL = "C";
        args = [ "-euc" command ];
      };
    in assert builtins.elem kind [ "public" "private" "ipv6" "ordinary" ];
      builtins.derivation (base // (if kind == "ordinary" then {} else {
        outputHashMode = "flat";
        outputHashAlgo = "sha256";
        outputHash = builtins.hashString "sha256" "ok";
      }))
  ')
  [[ $(<"$network_output") == ok ]]
done
export PROBE_UTIL_STORE
PROBE_UTIL_STORE=$(dirname "$(dirname "$(readlink -f "$(command -v fallocate)")")")
disk_output=$(setpriv --reuid=2000 --regid=2000 --clear-groups \
  nix-build --no-out-link --option substituters '' --expr '
  let
    bash = builtins.storePath (builtins.getEnv "PROBE_BASH_STORE");
    util = builtins.storePath (builtins.getEnv "PROBE_UTIL_STORE");
    core = builtins.storePath (builtins.getEnv "PROBE_CORE_STORE");
  in builtins.derivation {
    name = "wrix-pool-disk-admission";
    system = builtins.getEnv "PROBE_SYSTEM";
    builder = bash + "/bin/bash";
    LC_ALL = "C";
    args = [ "-euc" ("if denial=$(" + util + "/bin/fallocate --length=9663676416 allocation 2>&1); then exit 1; fi; " + core + "/bin/rm allocation; [[ $denial == *No?space?left?on?device* ]]; printf ok > $out") ];
  }
')
[[ $(<"$disk_output") == ok ]]
ln -s "$output" /nix/var/nix/gcroots/test-result
oom_count() {
  local event count
  while read -r event count; do
    if [[ "$event" == oom_kill ]]; then
      printf '%s\n' "$count"
      return
    fi
  done < /sys/fs/cgroup/wrix-nix-pool/memory.events
  return 1
}
before_oom=$(oom_count)
if setpriv --reuid=2000 --regid=2000 --clear-groups \
  nix-build --no-out-link --option substituters '' --expr '
  let bash = builtins.storePath (builtins.getEnv "PROBE_BASH_STORE");
  in builtins.derivation {
    name = "wrix-pool-memory-exhaustion";
    system = builtins.getEnv "PROBE_SYSTEM";
    builder = bash + "/bin/bash";
    args = [ "-euc" "value=xxxxxxxxxxxxxxxx; for ((i=0;i<30;i++)); do value=$value$value; done; printf unexpected > $out" ];
  }
'; then
  printf 'Unbounded native build memory unexpectedly succeeded.\n' >&2
  exit 1
fi
[[ $(oom_count) -gt "$before_oom" ]]
setpriv --reuid=2000 --regid=2000 --clear-groups nix store info
nix-store --check-validity "$output"
nix-store --gc
nix-store --check-validity "$output" "$bash_store" "$core_store"
[[ $(</sys/fs/cgroup/wrix-nix-pool/memory.max) == 1610612736 ]]
[[ $(</sys/fs/cgroup/wrix-nix-pool/pids.max) == 256 ]]
[[ $(</sys/fs/cgroup/wrix-nix-pool/cpu.max) == '200000 100000' ]]
kill "$(</run/wrix-nix-pool/daemon.pid)"
if wait "$bootstrap_pid"; then
  printf 'Native daemon stopped after completed fixture work.\n'
else
  [[ "$?" == 143 ]]
fi
[[ $(stat -c %s /var/lib/wrix/nix-pool/store.ext4) == 8589934592 ]]
[[ $(stat -f -c %T /nix/store) == ext2/ext3 ]]
before=$(sha256sum /nix/var/nix/db/db.sqlite)
if "$entrypoint" > /tmp/pool-retry.log 2>&1; then
  printf 'Uncertain owner was incorrectly readmitted.\n' >&2
  exit 1
else
  [[ "$?" == 73 ]]
fi
[[ $(sha256sum /nix/var/nix/db/db.sqlite) == "$before" ]]
printf 'Pool bootstrap, strict build, GC pins and retained-owner refusal passed.\n'
