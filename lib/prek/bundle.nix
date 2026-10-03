{ pkgs }:

pkgs.runCommand "wrix-prek-hooks"
  {
    outputHash = "sha256-NGZ6hFvtkMXD93fWE1NYPzAFnRChzFIt5ZJ3qvo8m8w=";
    outputHashAlgo = "sha256";
    outputHashMode = "recursive";
  }
  ''
    set -euo pipefail
    cp -a ${./hooks}/. "$out"
    cp ${./binding.sh} "$out/_binding.sh"
  ''
