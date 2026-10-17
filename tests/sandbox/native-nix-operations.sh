#!/usr/bin/env bash
set -euo pipefail

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"
export HOME="$work/home"
mkdir -p "$HOME" src
printf '[package]\nname="wrix-native-smoke"\nversion="0.0.0"\nedition="2024"\n' > Cargo.toml
printf '#[test]\nfn native_test() { assert_eq!(2 + 2, 4); }\n' > src/lib.rs

printf 'runtime uid=%s\n' "$(id -u)"
export LOOM_NATIVE_PREFLIGHT=rust
pi --help > pi-help
[[ -s pi-help ]]

bash_path=$(readlink -f "$(command -v bash)")
export WRIX_TEST_BASH="$bash_path"
cat > build.nix <<'NIX'
builtins.derivation {
  name = "wrix-native-additive";
  system = builtins.currentSystem;
  builder = builtins.getEnv "WRIX_TEST_BASH";
  args = [ "-c" "printf 'native-additive-ok' > \"$out\"" ];
}
NIX
nix build --offline --impure --file build.nix --no-link --print-out-paths > output-path
output=$(< output-path)
[[ "$(< "$output")" == native-additive-ok ]]
nix-store --check-validity "$output"
nix path-info "$output"

# A pinned nixpkgs source is an explicit fixture input, never a host store mount.
if [[ $# -ne 1 ]]; then
  printf 'usage: native-nix-operations.sh /fixture/nixpkgs\n' >&2
  exit 2
fi
export WRIX_TEST_NIXPKGS="$1"
nix develop --impure --expr \
  'let pkgs = import (builtins.getEnv "WRIX_TEST_NIXPKGS") {}; in pkgs.mkShellNoCC {}' \
  --command bash -c 'test -n "$IN_NIX_SHELL"'
printf 'native preflight, additive Nix build, and nix develop: PASS\n'
