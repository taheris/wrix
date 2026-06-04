#!/usr/bin/env bash
# Verifier for the in-container Nix criterion of specs/sandbox.md (FR #13,
# "In-container Nix (additive)"):
#
#   In a fresh container built from a profile that ships `nix`, the
#   unprivileged runtime user runs `nix develop -c true` and a `nix build`
#   of a flake target to completion (exit 0) with no `Operation not
#   permitted` failure on a `/nix/store` path.
#
# Exercises the LIVE runtime path — a real container started from the
# nix-shipping test image (`.#test-image-nix`, which adds `pkgs.nix` to the
# profile's packages), with the unprivileged runtime user driving real
# `nix develop` / `nix build` against the image's baked store. No host-side
# nix substitution stands in for the container path (no tier-skipping): the
# whole point is to prove the baked Nix DB registers the image's on-disk
# store with no orphan, so additive store writes by uid != 0 never hit
# EPERM (mechanism owned by image-builder.md § In-Container Nix Store
# Consistency).
#
# The container is launched the way the Linux launcher launches the default
# boundary (lib/sandbox/linux/default.nix): no `--userns=keep-id`, so the
# process is rootless container-root (the store owner) with
# `LD_PRELOAD=/lib/libfakeuid.so` spoofing uid 1000; `--passwd-entry` names it
# `wrapix`, and a `U=true` tmpfs at /home/wrapix gives Nix a writable HOME.
# The default entrypoint is bypassed (`--entrypoint /bin/bash`) so the probe
# is the focused additive-Nix path, not the agent bootstrap.
#
# The flake under test pins `nixpkgs` and exposes a `mkShellNoCC` devShell
# plus a small build target (`hello`). `nix develop` necessarily realizes a
# stdenv-shaped dev environment (Nix replaces the build with its own
# `get-env`, which a bare input-free derivation cannot satisfy), and both
# `nix develop` and `nix build` substitute their closures into the baked
# store as the unprivileged user — that substitution is the EPERM tripwire.
# Because the realistic dev-shell path needs a substituter, the probe first
# checks outbound reachability and SKIPS (exit 77) when the container has no
# network, rather than reporting a false failure offline.
#
#   Linux + rootless podman + nix  -> exercise the image
#   Darwin                         -> exit 77 (macOS path covered by tests/darwin/*)
#   non-Linux non-Darwin           -> exit 77
#   nix or podman missing          -> exit 77
#   container has no outbound net   -> exit 77 (probe self-skips)

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

WORKSPACE=$(mktemp -d -t wrapix-nix-in-container.XXXXXX)
cleanup() {
  rm -rf "$WORKSPACE"
  if podman image exists localhost/wrapix-nix:latest; then
    podman rmi localhost/wrapix-nix:latest >/dev/null
  fi
}
trap cleanup EXIT

IMAGE_REF="localhost/wrapix-nix:latest"
# Clear stale wrapix-nix images BEFORE load so the post-load retag has exactly
# one candidate (same retag pattern as container-starts.sh / shell.nix).
if podman image exists "$IMAGE_REF"; then
  podman rmi "$IMAGE_REF" >/dev/null
fi
"$IMAGE_STREAM" | podman load >/dev/null
loaded_id=$(podman images --quiet --filter "reference=*wrapix-nix*" | head -n1)
[[ -n "$loaded_id" ]] || { echo "FAIL: image not found after podman load" >&2; podman images >&2; exit 1; }
podman tag "$loaded_id" "$IMAGE_REF"

# The probe runs entirely inside the container so the flake resolves and
# realizes against the image's own Nix store, never the host's. It drives a
# real `nix develop -c true` (stdenv dev shell) and a real `nix build` as the
# unprivileged user; both substitute closures into the baked store, which is
# where a broken/orphaned Nix DB would surface EPERM. Exits 77 to self-skip
# when the container has no outbound network (the dev-shell path needs a
# substituter).
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

# Mirror the entrypoint: under root+libfakeuid, /workspace and nix's libgit2
# flake/tarball caches are owned by container-root while libgit2 sees uid 1000,
# so the git fetcher rejects them as "not owned by current user" without this.
git config --global --add safe.directory '*'

command -v nix >/dev/null 2>&1 || { echo "FAIL: nix not on PATH inside the container" >&2; exit 1; }

# The realistic nix-shipping-profile dev shell pulls a stdenv closure from a
# substituter; with no outbound network the probe cannot exercise the path, so
# self-skip (77) rather than report a false store-permission failure.
# 2>/dev/null drops the connection error itself — the probe reads reachability
# from the exit code, not stderr (SH-6).
net_ok() { local hp="$1"; timeout 12 bash -c "exec 3<>/dev/tcp/$hp/443" 2>/dev/null; }
if ! net_ok cache.nixos.org || ! net_ok github.com; then
  echo "PROBE-SKIP: no outbound network in container; in-container additive Nix needs a substituter" >&2
  exit 77
fi

arch=$(uname -m)
sys="${arch}-linux"

cd /workspace
cat > flake.nix <<NIXEOF
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs =
    { self, nixpkgs }:
    let
      pkgs = nixpkgs.legacyPackages.${sys};
    in
    {
      devShells.${sys}.default = pkgs.mkShellNoCC { };
      packages.${sys}.default = pkgs.hello;
    };
}
NIXEOF

echo "[probe] nix --version: $(nix --version)" >&2

# Both operations add paths to the baked store as the unprivileged user.
echo "[probe] nix develop -c true" >&2
nix develop -c true

echo "[probe] nix build .#default" >&2
nix build --no-link --print-out-paths ".#packages.${sys}.default" > /tmp/probe-out

out=$(cat /tmp/probe-out)
[[ -n "$out" ]] || { echo "FAIL: nix build produced no output path" >&2; exit 1; }
[[ -x "$out/bin/hello" ]] || { echo "FAIL: nix build target missing expected bin/hello: $out" >&2; exit 1; }
echo "[probe] built and registered $out" >&2

# Store-MUTATING op. The additive build above only substitutes a brand-new
# closure owned by the writer, so it never chmods a baked root-owned path —
# the real failure is create/replace/GC/delete -> deletePath -> fchmodat2(u+w)
# on a baked path. Reproduce that exact primitive on a baked /nix/store dir:
# under a plain uid-1000 mapping it fails EPERM; the runtime user must own the
# store (rootless container-root) to make it writable.
# -print -quit stops find at the first match: piping to `head` would SIGPIPE
# find (141) under `set -o pipefail` before the chmod runs (/nix/store is huge).
baked=$(find /nix/store -mindepth 1 -maxdepth 1 -type d -name '*-*' -print -quit)
[[ -n "$baked" ]] || { echo "FAIL: no baked /nix/store path found to mutate" >&2; exit 1; }
echo "[probe] chmod u+w baked store path (deletePath primitive): $baked" >&2
chmod u+w "$baked"
echo "[probe] mutated baked store path without EPERM" >&2

echo "PROBE-OK"
PROBE

# Mirror the launcher's default-boundary invocation: no keep-id (rootless
# container-root owns the store), LD_PRELOAD libfakeuid so tools see uid 1000,
# a wrapix passwd entry, and a writable tmpfs HOME (lib/sandbox/linux/default.nix).
set +e
output=$(podman run --rm --network=pasta \
  --passwd-entry "wrapix:*:$(id -u):$(id -g)::/home/wrapix:/bin/bash" \
  --mount type=tmpfs,destination=/home/wrapix,U=true \
  --entrypoint /bin/bash \
  -v "$WORKSPACE:/workspace:rw" \
  -w /workspace \
  -e HOME=/home/wrapix \
  -e LD_PRELOAD=/lib/libfakeuid.so \
  "$IMAGE_REF" \
  -c "$IN_CONTAINER" 2>&1)
status=$?
set -e

printf '%s\n' "$output" >&2

# The probe self-skips with 77 when the container has no substituter reachable.
if [[ $status -eq 77 ]]; then
  skip "in-container probe could not reach a substituter (no outbound network)"
fi

# Hard requirement 1: the runtime user completed nix develop + nix build.
[[ $status -eq 0 ]] || {
  echo "FAIL: in-container nix probe exited $status (expected 0)" >&2
  exit 1
}
[[ "$output" == *PROBE-OK* ]] || {
  echo "FAIL: probe did not reach PROBE-OK sentinel" >&2
  exit 1
}

# Hard requirement 2: no store-permission failure on a /nix/store path. A
# broken baked DB surfaces as `Operation not permitted` / EPERM while Nix
# tries to chmod or write an orphaned root-owned store path.
if printf '%s\n' "$output" | grep -nE '/nix/store' | grep -iE 'operation not permitted|EPERM' >/dev/null; then
  echo "FAIL: store-permission failure on a /nix/store path during additive Nix:" >&2
  printf '%s\n' "$output" | grep -iE 'operation not permitted|EPERM' >&2
  exit 1
fi

echo "PASS: nix-in-container (unprivileged runtime user ran nix develop + nix build; no /nix/store EPERM)" >&2
