#!/usr/bin/env bash
# Verifier for specs/sandbox.md (Image install path) and
# specs/image-builder.md (Success Criteria #1):
#
#   The launcher's image-install step is short-circuited when the
#   image's content digest is already present in the platform store:
#   no tar materialization, no stream invocation, no `*-load` CLI call.
#
# The actual shim podman + skopeo flow lives in
# tests/sandbox/image-checks.nix:imageInstallDigestSkipTest and is exposed
# as `nix run .#test-image-install-digest-skip`. This bash entry exists so
# the spec criterion `[system?](bash tests/sandbox/image-install-digest-skip.sh)`
# resolves; it delegates to the nix-built test.
#
#   Linux + nix   -> exercise the shim
#   Darwin        -> exit 77 (Darwin digest preflight covered by its own
#                    platform-resident verifier)
#   other         -> exit 77
#   nix missing   -> exit 77

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

skip() {
  echo "SKIP: $1" >&2
  exit 77
}

uname_s=$(uname -s)
[[ "$uname_s" = "Linux" ]] || skip "Linux-only verifier (uname=$uname_s); Darwin digest preflight covered separately"
command -v nix >/dev/null 2>&1 || skip "nix not on PATH"

cd "$REPO_ROOT"
exec nix run --no-warn-dirty .#test-image-install-digest-skip -- "$@"
