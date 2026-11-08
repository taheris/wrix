#!/usr/bin/env bash
set -euo pipefail
umask 077

[[ "$#" == 2 ]]
client="$1"
binding="$2"
scratch=$(mktemp -d)
scratch=$(cd "$scratch" && pwd -P)
trap 'chmod -R u+w "$scratch"; rm -rf "$scratch"' EXIT
controller=$(jq -r .controller "$binding")
runtime=$(jq -r .runtime "$binding")
controller_recipe=$(jq -r .controller_derivation "$binding")
runtime_recipe=$(jq -r .runtime_derivation "$binding")

nix-store --realise "$controller_recipe" "$runtime_recipe"
nix key generate-secret --key-name wrix-boundary-fixture > "$scratch/key"
public=$(nix key convert-secret-to-public < "$scratch/key")
nix copy --to "file://$scratch/cache?secret-key=$scratch/key" "$controller" "$runtime"
store="local?root=$scratch/store"
nix copy --derivation --to "$store" "$controller_recipe" "$runtime_recipe"

export HOME="$scratch/home"
mkdir -m 700 "$HOME"
export WRIX_CONTROLLER_PACKAGE_BINDING="$binding"
export NIX_REMOTE="$store"
export NIX_CONFIG="${NIX_CONFIG-}
substituters = file://$scratch/cache
trusted-public-keys = $public
"

"$client" --help
"$client" service capabilities
"$client" service --help

for output in "$controller" "$runtime"; do
  if nix-store --check-validity "$output"; then
    echo "ordinary client command unexpectedly realized host tooling" >&2
    exit 1
  fi
done

"$client" service prepare
for output in "$controller" "$runtime"; do
  nix-store --check-validity "$output"
  nix-store --query --roots "$output" > "$scratch/roots"
  grep -F 'prepared-controller/' "$scratch/roots"
done
[[ ! -e "$HOME/Library/Application Support/wrix/control/package.json" ]]
[[ ! -e "$HOME/.local/state/wrix/control/package.json" ]]
