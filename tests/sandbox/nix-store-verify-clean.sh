#!/usr/bin/env bash
# Verifier for the in-container store-cleanliness criterion of specs/sandbox.md
# (FR #13, "In-container Nix (additive)"):
#
#   A freshly provisioned container — with no prior store surgery — passes
#   `nix-store --verify --check-contents` with zero missing or dangling
#   paths, so an additive `nix build` cannot fail with `No such file or
#   directory` on a path the baked Nix DB registers as valid.
#
# The build-time guarantee is owned by image-builder.md § In-Container Nix
# Store Consistency: the baked Nix DB is registered over the MATERIALIZED
# contents closure (lib/sandbox/image.nix `imageNixDb`), so the
# registered-valid set equals the on-disk `/nix/store` set with no discrepancy
# in either direction. This verifier exercises the LIVE runtime path — a real
# container started from the nix-shipping test image (`.#test-image-nix`) — and
# asserts `nix-store --verify --check-contents` finds neither an orphan
# (on-disk but unregistered) nor a dangling registration (registered but
# absent). A dangling path is the failure this complements: `--check-contents`
# reports it as a disappeared/missing path, and an additive build that trusts
# the DB would dead-end on it with `No such file or directory`.
#
# The container is launched the way the Linux launcher launches the default
# boundary (lib/sandbox/linux/default.nix): no `--userns=keep-id`, so the
# process is rootless container-root (the store owner) with
# `LD_PRELOAD=/lib/libfakeuid.so` spoofing uid 1000, and `--passwd-entry` names
# it `wrapix`. The default entrypoint is bypassed (`--entrypoint /bin/bash`) so
# the probe is the focused store-verify path, not the agent bootstrap. No
# network is needed — `--verify` reads only the baked store and DB — so unlike
# nix-in-container.sh this verifier never self-skips on a missing substituter.
#
#   Linux + rootless podman + nix  -> exercise the image
#   Darwin                         -> exit 77 (macOS path covered by tests/darwin/*)
#   non-Linux non-Darwin           -> exit 77
#   nix or podman missing          -> exit 77

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

skip() {
  echo "SKIP: $1" >&2
  exit 77
}

uname_s=$(uname -s)
[[ "$uname_s" = "Linux" ]] || skip "Linux-only verifier (uname=$uname_s); macOS covered by tests/darwin/*"
command -v nix    >/dev/null 2>&1 || skip "nix not on PATH"
command -v podman >/dev/null 2>&1 || skip "podman not on PATH"
# Nested rootless podman can't load OCI images (overlayfs deadlock); skip vs hang.
[[ -e /run/.containerenv ]] && skip "nested container: podman load unavailable"

cd "$REPO_ROOT"

IMAGE_STREAM=$(nix build --no-link --print-out-paths --no-warn-dirty .#test-image-nix)

cleanup() {
  if podman image exists localhost/wrapix-nix:latest; then
    podman rmi localhost/wrapix-nix:latest >/dev/null
  fi
}
trap cleanup EXIT

IMAGE_REF="localhost/wrapix-nix:latest"
# Clear stale wrapix-nix images BEFORE load so the post-load retag has exactly
# one candidate (same retag pattern as nix-in-container.sh / container-starts.sh).
if podman image exists "$IMAGE_REF"; then
  podman rmi "$IMAGE_REF" >/dev/null
fi
"$IMAGE_STREAM" | podman load >/dev/null
loaded_id=$(podman images --quiet --filter "reference=*wrapix-nix*" | head -n1)
[[ -n "$loaded_id" ]] || { echo "FAIL: image not found after podman load" >&2; podman images >&2; exit 1; }
podman tag "$loaded_id" "$IMAGE_REF"

# The probe runs entirely inside the container so `nix-store --verify` reads the
# image's own baked store and DB, never the host's. `--check-contents` re-hashes
# every registered path; a dangling registration surfaces as a disappeared /
# missing path, an orphan as an unregistered on-disk path. Run as the
# unprivileged runtime user (uid != 0): a fresh container does no store surgery,
# so a clean DB must verify clean.
# `read -rd ''` returns non-zero at the heredoc's EOF after a full read; the
# `|| true` absorbs only that expected EOF status, not a real error (SH-6).
read -r -d '' IN_CONTAINER <<'PROBE' || true
set -euo pipefail
export HOME=/home/wrapix
export NIX_CONFIG="experimental-features = nix-command flakes"

uid=$(id -u)
if [[ "$uid" -eq 0 ]]; then
  echo "FAIL: probe is running as root; the criterion requires the unprivileged runtime user" >&2
  exit 1
fi
echo "[probe] running as uid=$uid ($(id -un 2>/dev/null || echo '?'))" >&2

command -v nix-store >/dev/null 2>&1 || { echo "FAIL: nix-store not on PATH inside the container" >&2; exit 1; }

# --verify --check-contents without --repair only REPORTS; it must find no
# missing/dangling path in a freshly provisioned store. Capture stdout+stderr
# and the exit status; assert both.
set +e
verify_out=$(nix-store --verify --check-contents 2>&1)
verify_status=$?
set -e
printf '%s\n' "$verify_out" >&2

if [[ $verify_status -ne 0 ]]; then
  echo "FAIL: nix-store --verify --check-contents exited $verify_status (expected 0)" >&2
  exit 1
fi

# Belt-and-suspenders: even if the exit code were 0, a reported missing/dangling
# path means the baked DB disagrees with the disk. `--verify` phrases an absent
# registered path as "path '...' disappeared" / "is missing".
if printf '%s\n' "$verify_out" | grep -iE "disappeared|is missing|missing path|not valid" >/dev/null; then
  echo "FAIL: nix-store --verify reported a missing/dangling registered path" >&2
  exit 1
fi

echo "PROBE-OK"
PROBE

# Mirror the launcher's default-boundary invocation: no keep-id (rootless
# container-root owns the store), LD_PRELOAD libfakeuid so tools see uid 1000,
# and a wrapix passwd entry (lib/sandbox/linux/default.nix). No network:
# --verify reads only the baked store.
set +e
output=$(podman run --rm --network=none \
  --passwd-entry "wrapix:*:$(id -u):$(id -g)::/home/wrapix:/bin/bash" \
  --entrypoint /bin/bash \
  -e HOME=/home/wrapix \
  -e LD_PRELOAD=/lib/libfakeuid.so \
  "$IMAGE_REF" \
  -c "$IN_CONTAINER" 2>&1)
status=$?
set -e

printf '%s\n' "$output" >&2

[[ $status -eq 0 ]] || {
  echo "FAIL: in-container nix-store --verify probe exited $status (expected 0)" >&2
  exit 1
}
[[ "$output" == *PROBE-OK* ]] || {
  echo "FAIL: probe did not reach PROBE-OK sentinel" >&2
  exit 1
}

echo "PASS: nix-store-verify-clean (fresh container store verifies with no missing/dangling path)" >&2
