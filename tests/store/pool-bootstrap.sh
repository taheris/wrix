#!/usr/bin/env bash
set -euo pipefail
[[ "$#" == 1 ]]
entrypoint=$1

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
  if nix store info; then
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
  'max-jobs = 2' 'cores = 2' 'require-sigs = true'; do
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
ln -s "$output" /nix/var/nix/gcroots/test-result
nix-store --gc
nix-store --check-validity "$output" "$bash_store" "$core_store"
read -ra children <<< "$(<"/proc/$bootstrap_pid/task/$bootstrap_pid/children")"
[[ "${#children[@]}" == 1 ]]
kill "${children[0]}"
if wait "$bootstrap_pid"; then
  printf 'Native daemon stopped after completed fixture work.\n'
else
  [[ "$?" == 143 ]]
fi
before=$(sha256sum /var/lib/wrix/nix-pool/nix/var/nix/db/db.sqlite)
if "$entrypoint" > /tmp/pool-retry.log 2>&1; then
  printf 'Uncertain owner was incorrectly readmitted.\n' >&2
  exit 1
else
  [[ "$?" == 73 ]]
fi
[[ $(sha256sum /var/lib/wrix/nix-pool/nix/var/nix/db/db.sqlite) == "$before" ]]
printf 'Pool bootstrap, strict build, GC pins and retained-owner refusal passed.\n'
