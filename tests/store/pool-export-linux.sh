#!/usr/bin/env bash
set -euo pipefail
umask 077

[[ $(uname -s) == Linux && -r /dev/kvm && -w /dev/kvm && -r /dev/fuse && -w /dev/fuse ]]
root=$(mktemp -d /tmp/wrix-pool-export.XXXXXXXX)
name="wrix-pool-export-${root##*.}"
name=${name,,}
ref="localhost/$name:fixture"
started=false
loaded=false
mkdir -m 0700 "$root/data" "$root/view"

cleanup() {
  local status=$? unmounted=true
  trap - EXIT
  if mountpoint -q "$root/view"; then
    if ! fusermount3 -u "$root/view"; then
      printf 'Export is still busy; retaining its VM and fixture at %s\n' "$root" >&2
      unmounted=false
      status=1
    fi
  fi
  if [[ "$started" == true ]]; then
    podman inspect "$name" --format '{{json .State}}' > "$root/runtime-state.json" || status=1
    podman logs "$name" > "$root/service.log" 2>&1 || status=1
    if [[ "$unmounted" == true ]]; then
      podman stop --time=30 "$name" > "$root/stop.log" 2>&1 || status=1
      podman rm "$name" > "$root/remove.log" 2>&1 || status=1
    fi
  fi
  if [[ "$loaded" == true && "$unmounted" == true ]]; then
    podman image rm "$ref" > "$root/image-remove.log" 2>&1 || status=1
  fi
  if [[ "$status" == 0 ]]; then
    # Only this stopped test fixture is removed; this is not pool maintenance.
    rm -rf "$root"
  else
    printf 'Pool export fixture evidence: %s\n' "$root" >&2
  fi
  exit "$status"
}
trap cleanup EXIT

ssh-keygen -q -t ed25519 -N '' -f "$root/admin"
ssh-keygen -q -t ed25519 -N '' -f "$root/client"
read -r key_type key_blob _comment < "$root/admin.pub"
jq -n --arg key "$key_type $key_blob" '{version:1,admin_key:$key}' > "$root/data/bootstrap.json"
layout=$(jq -er .oci_layout "$WRIX_TEST_POOL_SOURCE")
oci_ref=$(jq -er .oci_ref "$WRIX_TEST_POOL_SOURCE")
skopeo --insecure-policy copy --quiet "oci:$layout:$oci_ref" "docker-archive:$root/image.tar:$ref"
podman load -i "$root/image.tar" > "$root/load.log" 2>&1
loaded=true
podman run -d --name "$name" --runtime "$WRIX_TEST_POOL_RUNTIME" \
  --memory=2g --cpus=2 --pids-limit=256 --log-opt=max-size=16m \
  -p 127.0.0.1::2222 -v "$root/data:/var/lib/wrix/nix-pool:rw" "$ref" > "$root/container.id"
started=true
endpoint=$(podman port "$name" 2222/tcp)
[[ "$endpoint" == 127.0.0.1:* && "$endpoint" != *$'\n'* ]]
port=${endpoint#127.0.0.1:}
[[ "$port" =~ ^[0-9]+$ && "$port" -gt 0 && "$port" -le 65535 ]]
for _attempt in {1..600}; do
  if [[ -f "$root/data/host-key.pub" ]]; then break; fi
  [[ $(podman inspect "$name" --format '{{.State.Running}}') == true ]]
  sleep 0.1
done
read -r key_type key_blob _comment < "$root/data/host-key.pub"
printf '[127.0.0.1]:%s %s %s\n' "$port" "$key_type" "$key_blob" > "$root/hosts"
ssh_options=(-F /dev/null -p "$port" -o BatchMode=yes -o IdentitiesOnly=yes \
  -o IdentityAgent=none -o StrictHostKeyChecking=yes -o "UserKnownHostsFile=$root/hosts" \
  -o ConnectTimeout=2 -o ConnectionAttempts=1)
pool_control() {
  ssh "${ssh_options[@]}" -i "$root/admin" root@127.0.0.1 control
}
ready=false
for _attempt in {1..600}; do
  if reply=$(printf '{"operation":"inspect"}\n' | pool_control 2> "$root/startup-ssh.log"); then
    [[ "$reply" == *'"result":"ready"'* ]]
    ready=true
    break
  fi
  [[ $(podman inspect "$name" --format '{{.State.Running}}') == true ]]
  sleep 0.1
done
[[ "$ready" == true ]]
read -r key_type key_blob _comment < "$root/client.pub"
reply=$(printf '{"operation":"register","id":"11111111111111111111111111111111","key":"%s %s"}\n' "$key_type" "$key_blob" | pool_control)
[[ "$reply" == *'"result":"applied"'* ]]
export NIX_REMOTE=ssh-ng://nix-client@127.0.0.1
export NIX_SSHOPTS="-F /dev/null -p $port -i $root/client -o BatchMode=yes -o IdentitiesOnly=yes -o IdentityAgent=none -o StrictHostKeyChecking=yes -o UserKnownHostsFile=$root/hosts"
info=$(nix store info --json)
jq -e '.trusted == false' <<< "$info" > "$root/trust-check"
sshfs -p "$port" -o ro -o cache=no -o attr_timeout=0 -o entry_timeout=0 \
  -o negative_timeout=0 -o 'ssh_command=ssh -F /dev/null' -o "IdentityFile=$root/client" \
  -o BatchMode=yes -o IdentitiesOnly=yes -o IdentityAgent=none -o StrictHostKeyChecking=yes \
  -o "UserKnownHostsFile=$root/hosts" nix-export@127.0.0.1:/nix/store "$root/view"
mountpoint -q "$root/view"
expr='{ bash }: builtins.derivation {
  name = "wrix-live-pool-export";
  system = builtins.currentSystem;
  builder = (builtins.storePath bash) + "/bin/bash";
  args = [ "-c" "printf live > $out" ];
}'
drv=$(nix-instantiate --option substituters '' --argstr bash "$WRIX_TEST_POOL_BASH" --expr "$expr")
output=$(nix-store -q --outputs "$drv")
[[ "$output" == /nix/store/* && "$output" != *$'\n'* ]]
view_path="$root/view/${output#/nix/store/}"
[[ ! -e "$view_path" ]]
nix-store -r "$drv" --option substituters '' > "$root/realization"
[[ $(< "$view_path") == live ]]
dns_output=$(nix-build --no-out-link --option substituters '' --argstr bash "$WRIX_TEST_POOL_BASH" --expr '
  { bash }: builtins.derivation {
    name = "wrix-pool-public-dns";
    system = builtins.currentSystem;
    builder = (builtins.storePath bash) + "/bin/bash";
    args = [ "-c" "exec 3<>/dev/tcp/cache.nixos.org/443; printf dns-ok > $out" ];
    outputHashMode = "flat";
    outputHashAlgo = "sha256";
    outputHash = builtins.hashString "sha256" "dns-ok";
  }
')
[[ $(< "$root/view/${dns_output#/nix/store/}") == dns-ok ]]
if timeout --kill-after=1s 3s nix-store --gc > "$root/client-gc.log" 2>&1; then
  printf 'Native client collection bypassed live filesystem protection.\n' >&2
  exit 1
else
  [[ "$?" == 124 ]]
fi
gc_output=$(< "$root/client-gc.log")
[[ "$gc_output" == *'waiting for the big garbage collector lock'* ]]
[[ $(< "$view_path") == live ]]
if touch "$root/view/forbidden-write" > "$root/write.log" 2>&1; then
  printf 'The filesystem export permitted mutation.\n' >&2
  exit 1
fi
reply=$(printf '{"operation":"collect"}\n' | pool_control)
[[ "$reply" == *'"result":"busy"'* ]]
reply=$(printf '{"operation":"shutdown"}\n' | pool_control)
[[ "$reply" == *'"result":"busy"'* ]]
reply=$(printf '{"operation":"release","id":"11111111111111111111111111111111"}\n' | pool_control)
[[ "$reply" == *'"result":"busy"'* ]]
[[ $(< "$view_path") == live ]]
fusermount3 -u "$root/view"
released=false
for _attempt in {1..200}; do
  reply=$(printf '{"operation":"release","id":"11111111111111111111111111111111"}\n' | pool_control)
  if [[ "$reply" == *'"result":"applied"'* ]]; then released=true; break; fi
  [[ "$reply" == *'"result":"busy"'* ]]
  sleep 0.05
done
[[ "$released" == true ]]
reply=$(printf '{"operation":"collect"}\n' | pool_control)
[[ "$reply" == *'"result":"applied"'* ]]
if reply=$(printf '{"operation":"shutdown"}\n' | pool_control 2> "$root/shutdown-ssh.log"); then
  if [[ -n "$reply" ]]; then
    [[ "$reply" == *'"result":"stopping"'* ]]
  else
    printf 'Shutdown ended its transport before the acknowledgement; checking native release.\n'
  fi
else
  # Shutdown can close its own SSH transport; only observed VM exit proves completion.
  printf 'Shutdown transport closed; checking native release and runtime completion.\n'
fi
timeout --kill-after=1s 60s podman wait "$name" > "$root/runtime-exit"
[[ $(< "$root/runtime-exit") == 0 ]]
state=$(podman inspect "$name" --format '{{.State.Status}} {{.State.ExitCode}} {{.State.Running}}')
printf '%s\n' "$state" > "$root/shutdown-observation"
[[ "$state" == 'exited 0 false' || "$state" == 'stopped 0 false' ]]
[[ $(< "$root/data/released") == 1 ]]
printf 'Actual krun service VM, pinned SSH, read-only Host FUSE view, dynamic negative-lookup invalidation and drained shutdown passed.\n'
