#!/usr/bin/env bash
set -euo pipefail

: "${out:?}" "${NIX_BUILD_TOP:?}"
test ! -e "$out"
test ! -L "$out"
tool="$1"
configuration="$2"
shift 2
work="$(mktemp -d "$NIX_BUILD_TOP/wrix-oci.XXXXXXXX")"
trap 'rm -rf "$work"' EXIT
"$tool" oci-layout "$configuration" "$work/layout" "$@"
# Nix locks the final output; unregistered siblings in the store are GC candidates.
mv -T "$work/layout" "$out"
