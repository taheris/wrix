#!/usr/bin/env bash
set -euo pipefail

# Only Nix builders call this entrypoint: their output is a GC-protected build
# root before registration. Standalone, unreserved destinations use
# oci-layout-build.sh instead.
: "${out:?}"
test ! -e "$out"
test ! -L "$out"
tool="$1"
configuration="$2"
shift 2
mkdir "$out"
cleanup() {
  local status="$?"
  if [[ "$status" -ne 0 ]]; then rm -rf -- "$out"; fi
}
trap cleanup EXIT
"$tool" oci-layout "$configuration" "$out/.result" "$@"
shopt -s dotglob nullglob
entries=("$out/.result/"*)
mv -- "${entries[@]}" "$out/"
rmdir "$out/.result"
