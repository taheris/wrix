#!/usr/bin/env bash
set -euo pipefail
[[ "$#" == 1 ]]
entrypoint=$1

show_failure_logs() {
  local status=$? drv
  if [[ -f /tmp/pool-bootstrap.log ]]; then
    cat /tmp/pool-bootstrap.log
  fi
  for drv in /nix/store/*-wrix-pool-*.drv; do
    if [[ -f "$drv" ]]; then
      if ! NIX_REMOTE=daemon nix log "$drv"; then
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
ssh-keygen -q -t ed25519 -N "" -C "" -f /tmp/pool-admin
ssh-keygen -q -t ed25519 -N "" -C "" -f /tmp/pool-client
read -r key_type key_blob _comment < /tmp/pool-admin.pub
printf '{"version":1,"admin_key":"%s %s"}\n' "$key_type" "$key_blob" > /var/lib/wrix/nix-pool/bootstrap.json
chmod 0600 /var/lib/wrix/nix-pool/bootstrap.json

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
  if [[ -f /run/wrix-nix-pool/ready ]] && nix store info; then
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
read -r key_type key_blob _comment < /var/lib/wrix/nix-pool/host-key.pub
printf '[127.0.0.1]:2222 %s %s\n' "$key_type" "$key_blob" > /tmp/pool-hosts
pool_control() {
  ssh -i /tmp/pool-admin -p 2222 -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes \
    -o UserKnownHostsFile=/tmp/pool-hosts -o BatchMode=yes root@127.0.0.1 control
}
ssh_ready=false
for _attempt in {1..100}; do
  if reply=$(printf '{"operation":"inspect"}\n' | pool_control 2>/tmp/pool-ssh-startup); then
    [[ "$reply" == *ready* ]]
    ssh_ready=true
    break
  fi
  if ! kill -0 "$bootstrap_pid"; then
    cat /tmp/pool-bootstrap.log /tmp/pool-ssh-startup
    exit 1
  fi
  sleep 0.1
done
if [[ "$ssh_ready" != true ]]; then
  cat /tmp/pool-bootstrap.log /tmp/pool-ssh-startup
  exit 1
fi
seed_image=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
nix-store --export "$bash_store" > "/var/lib/wrix/nix-pool/incoming/$seed_image.nar"
chmod 0600 "/var/lib/wrix/nix-pool/incoming/$seed_image.nar"
seed_reply=$(printf '{"operation":"seed","image":"%s","paths":["%s"]}\n' "$seed_image" "$bash_store" | pool_control)
[[ "$seed_reply" == *applied* ]]
[[ $(readlink "/nix/var/nix/gcroots/images/$seed_image/$(basename "$bash_store")") == "$bash_store" ]]
rm "/var/lib/wrix/nix-pool/incoming/$seed_image.nar"
unsigned_output=$(nix-build --no-out-link --option substituters '' --argstr bash "$bash_store" --expr '
  { bash }: builtins.derivation {
    name = "wrix-pool-unsigned-input-addressed";
    system = builtins.currentSystem;
    builder = (builtins.storePath bash) + "/bin/bash";
    args = [ "-c" "printf unsigned > $out" ];
  }
')
nix-store --export "$unsigned_output" > /tmp/pool-unsigned.nar
reply=$(printf '{"operation":"collect"}\n' | pool_control)
[[ "$reply" == *applied* ]]
if nix-store --check-validity "$unsigned_output"; then
  printf 'Unsigned fixture was not removed before the native import denial probe.\n' >&2
  exit 1
fi
read -r key_type key_blob _comment < /tmp/pool-client.pub
printf '{"operation":"register","id":"11111111111111111111111111111111","key":"%s %s"}\n' "$key_type" "$key_blob" | pool_control
export NIX_SSHOPTS='-i /tmp/pool-client -p 2222 -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/tmp/pool-hosts -o BatchMode=yes'
export NIX_REMOTE=ssh-ng://nix-client@127.0.0.1
architecture=$(uname -m)
case "$architecture" in
  x86_64 | aarch64) export PROBE_SYSTEM="$architecture-linux" ;;
  *) printf 'Unsupported guest architecture: %s\n' "$architecture" >&2; exit 1 ;;
esac
export PROBE_BASH_STORE="$bash_store" PROBE_CORE_STORE="$core_store"
if info=$(nix store info 2>&1); then
  printf '%s\n' "$info"
else
  printf '%s\n' "$info" >&2
  exit 1
fi
[[ "$info" == *'Trusted: 0'* ]]
reply=$(printf '{"operation":"collect"}\n' | pool_control)
[[ "$reply" == *busy* ]]
if wrong_role=$(ssh -i /tmp/pool-client -p 2222 -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes \
  -o UserKnownHostsFile=/tmp/pool-hosts -o BatchMode=yes root@127.0.0.1 control </dev/null 2>&1); then
  printf 'Worker key acquired administration authority.\n' >&2
  exit 1
fi
[[ "$wrong_role" == *'Permission denied'* ]]
if import_reply=$(nix-store --import < /tmp/pool-unsigned.nar 2>&1); then
  printf 'Untrusted client imported an unsigned input-addressed path.\n' >&2
  exit 1
fi
[[ "$import_reply" == *'lacks a signature'* ]]
startup_image=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
install -m 0600 /tmp/pool-unsigned.nar "/var/lib/wrix/nix-pool/incoming/$startup_image.nar"
reply=$(printf '{"operation":"seed","image":"%s","paths":["%s"]}\n' "$startup_image" "$unsigned_output" | pool_control)
[[ "$reply" == *applied* ]]
nix-store --check-validity "$unsigned_output"
rm "/var/lib/wrix/nix-pool/incoming/$startup_image.nar"
output=$(nix-build --no-out-link --option substituters '' --option sandbox false \
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
if repair_reply=$(nix-store --verify --check-contents --repair 2>&1); then
  printf 'Untrusted client received store repair authority.\n' >&2
  exit 1
fi
if [[ "$repair_reply" != *'not privileged'* ]]; then
  printf '%s\n' "$repair_reply" >&2
  exit 1
fi
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
  network_output=$(nix-build --no-out-link --option substituters '' --expr '
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
disk_output=$(nix-build --no-out-link --option substituters '' --expr '
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
if nix-build --no-out-link --option substituters '' --expr '
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
nix store info
ssh -i /tmp/pool-client -p 2222 -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes \
  -o UserKnownHostsFile=/tmp/pool-hosts nix-export@127.0.0.1 'touch /tmp/export-shell-executed' </dev/null
[[ ! -e /tmp/export-shell-executed ]]
printf 'get %s /tmp/pool-read\n' "$output" | sftp -b - -i /tmp/pool-client -P 2222 \
  -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/tmp/pool-hosts nix-export@127.0.0.1
[[ $(</tmp/pool-read) == $(<"$output") ]]
if printf 'put /tmp/pool-read %s\n' "$output" | sftp -b - -i /tmp/pool-client -P 2222 \
  -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/tmp/pool-hosts nix-export@127.0.0.1; then
  printf 'Read-only export accepted mutation.\n' >&2
  exit 1
fi
printf readable-outside-jail > /tmp/export-outside-control
chmod 0644 /tmp/export-outside-control
if printf 'get /tmp/export-outside-control /tmp/stolen-world-readable\n' | sftp -b - -i /tmp/pool-client -P 2222 \
  -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/tmp/pool-hosts nix-export@127.0.0.1; then
  printf 'Export escaped its filesystem jail.\n' >&2
  exit 1
fi
if printf 'get /var/lib/wrix/nix-pool/bootstrap.json /tmp/stolen\n' | sftp -b - -i /tmp/pool-client -P 2222 \
  -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=/tmp/pool-hosts nix-export@127.0.0.1; then
  printf 'Export exposed private administration.\n' >&2
  exit 1
fi
released=false
for _attempt in {1..200}; do
  reply=$(printf '{"operation":"release","id":"11111111111111111111111111111111"}\n' | pool_control)
  if [[ "$reply" == *applied* ]]; then released=true; break; fi
  [[ "$reply" == *busy* ]]
  sleep 0.05
done
[[ "$released" == true ]]
export NIX_REMOTE=daemon
nix-store --check-validity "$output"
reply=$(printf '{"operation":"collect"}\n' | pool_control)
[[ "$reply" == *applied* ]]
nix-store --check-validity "$output" "$bash_store" "$core_store" "$unsigned_output"
[[ $(</sys/fs/cgroup/wrix-nix-pool/memory.max) == 1610612736 ]]
[[ $(</sys/fs/cgroup/wrix-nix-pool/pids.max) == 256 ]]
[[ $(</sys/fs/cgroup/wrix-nix-pool/control/pids.max) == 96 ]]
[[ $(</sys/fs/cgroup/wrix-nix-pool/control/memory.max) == 402653184 ]]
[[ $(</sys/fs/cgroup/wrix-nix-pool/cpu.max) == '200000 100000' ]]
supervisor_pid=$(</run/wrix-nix-pool/supervisor.pid)
kill -STOP "$supervisor_pid"
supervisor_stopped=false
for _attempt in {1..200}; do
  supervisor_status=$(<"/proc/$supervisor_pid/stat")
  if [[ "$supervisor_status" == *') T '* ]]; then supervisor_stopped=true; break; fi
  sleep 0.01
done
[[ "$supervisor_stopped" == true ]]
owner_pid=$(</run/wrix-nix-pool/owner.pid)
kill -KILL "$owner_pid"
owner_dead=false
for _attempt in {1..200}; do
  if [[ ! -e "/proc/$owner_pid/stat" ]]; then owner_dead=true; break; fi
  owner_status=$(<"/proc/$owner_pid/stat")
  if [[ "$owner_status" == *') Z '* ]]; then owner_dead=true; break; fi
  sleep 0.01
done
[[ "$owner_dead" == true ]]
if flock --nonblock --exclusive /nix/var/nix/gc.lock true; then
  printf 'Controller loss dropped the independent native GC protection.\n' >&2
  exit 1
fi
kill -CONT "$supervisor_pid"
if wait "$bootstrap_pid"; then
  printf 'Controller loss did not fail-stop the service.\n' >&2
  exit 1
else
  [[ "$?" == 137 ]]
fi
[[ $(stat -c %s /var/lib/wrix/nix-pool/store.ext4) == 8589934592 ]]
[[ $(stat -f -c %T /nix/store) == ext2/ext3 ]]
[[ ! -e /var/lib/wrix/nix-pool/released ]]
before=$(sha256sum /nix/var/nix/db/db.sqlite)
if "$entrypoint" > /tmp/pool-retry.log 2>&1; then
  printf 'Uncertain owner was incorrectly readmitted.\n' >&2
  exit 1
else
  [[ "$?" == 73 ]]
fi
[[ $(sha256sum /nix/var/nix/db/db.sqlite) == "$before" ]]
printf 'Pool bootstrap, strict build, GC pins and retained-owner refusal passed.\n'
