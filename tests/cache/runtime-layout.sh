#!/usr/bin/env bash
set -euo pipefail

# Only run beneath the disposable store harness and its private mount namespace.
[[ -f /nix/var/nix/wrix-test-isolated-store ]]
mount --make-rprivate /
mount -t tmpfs tmpfs /var
mkdir -p /var/lib/wrix/celler /var/cache/wrix/celler
ip link set lo up
export WRIX_TEST_RUNTIME_NAMESPACE=1
exec "$@"
