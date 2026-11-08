#!/usr/bin/env bash
set -euo pipefail
umask 077

[[ "$#" == 3 ]]
catalogue="$1"
profile="$2"
agent="$3"
scratch=$(mktemp -d)
scratch=$(cd "$scratch" && pwd -P)
trap 'chmod -R u+w "$scratch"; rm -rf "$scratch"' EXIT

jq -e --arg profile "$profile" --arg agent "$agent" \
  '.schema == 1 and .profiles[$profile][$agent] != null' "$catalogue"
recipe=$(jq -r --arg profile "$profile" --arg agent "$agent" \
  '.profiles[$profile][$agent].realization.derivation' "$catalogue")
selected=$(jq -r --arg profile "$profile" --arg agent "$agent" \
  '.profiles[$profile][$agent].realization.path' "$catalogue")
nix-store --query --outputs "$recipe" > "$scratch/actual"
printf '%s\n' "$selected" > "$scratch/expected"
cmp "$scratch/expected" "$scratch/actual"
nix-store --realise "$recipe" --add-root "$scratch/source-root" --indirect

nix key generate-secret --key-name wrix-catalogue-fixture > "$scratch/key"
public=$(nix key convert-secret-to-public < "$scratch/key")
nix copy --to "file://$scratch/cache?secret-key=$scratch/key" "$selected" "$catalogue"
store="local?root=$scratch/store"
nix copy --option trusted-public-keys "$public" --from "file://$scratch/cache" --to "$store" "$catalogue"
jq -r '.profiles[][].realization.path' "$catalogue" > "$scratch/entries"
while IFS= read -r output; do
  if nix-store --store "$store" --check-validity "$output"; then
    echo "catalogue retained a realized image entry" >&2
    exit 1
  fi
done < "$scratch/entries"

export NIX_REMOTE="$store"
export NIX_CONFIG="${NIX_CONFIG-}
substituters = file://$scratch/cache
trusted-public-keys = $public
"
nix-store --realise "$recipe" --add-root "$scratch/selected-root" --indirect
nix-store --check-validity "$selected"
nix-store --query --roots "$selected" > "$scratch/roots"
grep -F "$scratch/selected-root" "$scratch/roots"
manifest="$scratch/store$selected"
entry=$(jq -c --arg profile "$profile" --arg agent "$agent" '.profiles[$profile][$agent]' "$catalogue")
jq -e --arg profile "$profile" --arg agent "$agent" --argjson entry "$entry" \
  'keys == [$profile] and (.[$profile] | keys) == [$agent] and .[$profile][$agent].ref == $entry.ref and .[$profile][$agent].source_kind == $entry.source_kind' "$manifest"
jq -r --arg profile "$profile" --arg agent "$agent" \
  '.[$profile][$agent] | .source, .profile_config' "$manifest" > "$scratch/selected-inputs"
nix-store --store "$store" --gc
while IFS= read -r output; do
  nix-store --check-validity "$output"
  nix-store --query --roots "$output" > "$scratch/input-roots"
  grep -F "$scratch/selected-root" "$scratch/input-roots"
done < "$scratch/selected-inputs"
while IFS= read -r output; do
  if [[ "$output" != "$selected" ]] && nix-store --check-validity "$output"; then
    echo "selected provisioning realized another catalogue entry" >&2
    exit 1
  fi
done < "$scratch/entries"
