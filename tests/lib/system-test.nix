{
  pkgs,
  system,
  name,
  test,
}:

let
  inherit (pkgs.lib) escapeShellArg makeBinPath;
  inherit (import ./verifier.nix) nixosVm;
  runtimePath = makeBinPath [
    pkgs.coreutils
    pkgs.git
    pkgs.jq
    pkgs.nix
  ];
in
pkgs.writeShellScriptBin name ''
  set -euo pipefail
  export PATH="${runtimePath}:$PATH"
  source ${./verifier.sh}
  verifier_preflight "$(verifier_platform)" ${escapeShellArg (builtins.toJSON nixosVm)} >&2

  repo_root=$(git rev-parse --show-toplevel)
  if ! driver=$(nix build --no-link --print-out-paths --no-warn-dirty \
    "$repo_root#legacyPackages.${system}.systemTests.${test}.driver"); then
    exit 1
  fi
  directory=$(mktemp -d -t wrix-system-test.XXXXXX)
  trap 'rm -rf "$directory"' EXIT
  cd "$directory"
  if ! "$driver/bin/nixos-test-driver" -o "$directory"; then
    exit 1
  fi
''
