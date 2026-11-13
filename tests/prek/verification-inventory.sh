#!/usr/bin/env bash
set -euo pipefail

verification_inventory() {
  local repository="$1" system="$2" work="$3" identity scope directory output recipe fingerprint cacheable=0
  nix flake metadata --json "$repository" > "$work/source.json" || return 1
  jq -e '(.locked.rev | type) == "string" and (.url | type) == "string"' "$work/source.json" > /dev/null || return 1
  if jq -e '(.locked.narHash | type) == "string" and all(.locks.nodes[]; .locked == null or (.locked.narHash | type) == "string")' "$work/source.json" > /dev/null; then
    cacheable=1
  fi
  {
    nix --version || return 1
    printf '%s\n' "$system"
    jq -cS '{locked, locks, path}' "$work/source.json" || return 1
    nix config show --json | jq -cS 'with_entries(select(.key | IN("pure-eval", "eval-system", "restrict-eval", "allow-import-from-derivation", "allow-unsafe-native-code-during-evaluation", "experimental-features")))' || return 1
  } > "$work/identity"
  fingerprint=$(sha256sum "$work/identity") || return 1
  fingerprint=${fingerprint%% *}
  scope=$(printf '%s' "$repository" | sha256sum) || return 1
  scope=${scope%% *}
  directory="${XDG_CACHE_HOME:-$HOME/.cache}/wrix/verification-inventory/$scope/$system"
  (umask 077; mkdir -p "$directory") || return 1
  if [[ "$cacheable" -eq 1 && -f "$directory/record.json" && -L "$directory/current" ]] &&
    jq -e --arg key "$fingerprint" '.version == 1 and .fingerprint == $key and (.path | type) == "string"' "$directory/record.json" > /dev/null; then
    output=$(jq -r .path "$directory/record.json")
    if [[ "$output" == /nix/store/*-verification-inventory.json && "$(readlink "$directory/current")" == "$output" ]] &&
      nix-store --check-validity "$output"; then
      nix-store --add-root "$work/inventory-root" --indirect --realise "$output" > /dev/null || return 1
      printf '%s\n' "$output"
      return 0
    fi
  fi
  recipe=$(nix eval --raw --no-warn-dirty --expr '
    { reference, system }:
    let scope = (builtins.getFlake reference).legacyPackages.${system};
    in if scope ? verificationInventoryRecipe then scope.verificationInventoryRecipe else ""
  ' --argstr reference "$(jq -r .url "$work/source.json")" --argstr system "$system") || return 1
  [[ -n "$recipe" ]] || return 1
  if ! nix-store --check-validity "$recipe"; then
    recipe=$(nix eval --raw --option eval-cache false --no-warn-dirty "$repository#legacyPackages.$system.verificationInventoryRecipe") || return 1
  fi
  output=$(nix build --print-out-paths --no-warn-dirty --out-link "$work/inventory-root" "$recipe^*") || return 1
  [[ "$output" == /nix/store/*-verification-inventory.json && -f "$output" ]] || return 1
  if [[ "$cacheable" -eq 1 ]]; then
    nix-store --add-root "$directory/current" --indirect --realise "$output" > /dev/null || return 1
    identity=$(mktemp "$directory/record.XXXXXX") || return 1
    jq -n --arg fingerprint "$fingerprint" --arg path "$output" '{version:1, fingerprint:$fingerprint, path:$path}' > "$identity" || return 1
    mv -f "$identity" "$directory/record.json" || return 1
  fi
  printf '%s\n' "$output"
}
