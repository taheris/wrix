{ pkgs }:

pkgs.runCommand "wrix-prek-hooks"
  {
    outputHash = "sha256-WkgVCHKmroJq8GvaLVQ5uWlTzNyMaoTgATKybO6oRXg=";
    outputHashAlgo = "sha256";
    outputHashMode = "recursive";
  }
  ''
    set -euo pipefail
    mkdir -p "$out"
    cp -a ${./hooks}/* "$out/"
    cp ${./binding.sh} "$out/_binding.sh"
  ''
