#!/usr/bin/env bash
# Runs through the real Darwin image entrypoint, not a simulated UID mapping.
set -euo pipefail

[[ "$(id -u)" == 0 ]]
[[ -z "${LD_PRELOAD:-}" ]]
[[ "${IS_SANDBOX:-}" == 1 ]]
[[ "$(stat -c %u /workspace)" == 0 ]]
[[ "$(stat -c %u /workspace/.git)" == 0 ]]
for path in /nix /nix/var /nix/var/nix; do
  [[ "$(stat -c %u:%g:%a "$path")" == 0:0:755 ]]
done
[[ ! -e /homeless-shelter ]]
[[ ! -e /nix/var/nix/daemon-socket/socket ]]
[[ "$(git -C /workspace rev-parse --show-toplevel)" == /workspace ]]
printf 'guest write\n' >/workspace/.uid-mapping-test
[[ "$(stat -c %u /workspace/.uid-mapping-test)" == 0 ]]
touch "$HOME/.uid-test"
rm "$HOME/.uid-test"

nix --offline eval --expr '1 + 1' | grep -qx 2
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
WRIX_TEST_BASH="$(readlink -f "$(command -v bash)")"
export WRIX_TEST_BASH
cat >"$work/build.nix" <<'NIX'
builtins.derivation {
  name = "wrix-darwin-private-nix";
  system = builtins.currentSystem;
  builder = builtins.getEnv "WRIX_TEST_BASH";
  args = [ "-c" "printf 'private-nix-ok' > \"$out\"" ];
}
NIX
output=$(nix build --offline --impure --file "$work/build.nix" --no-link --print-out-paths)
[[ "$(<"$output")" == private-nix-ok ]]
nix-store --check-validity "$output"
printf 'PASS: real guest identity, VirtioFS writes, Nix evaluation and additive build\n'
