{ pkgs }:
let
  inventory = pkgs.writeText "verification-inventory.json" "{}";
  recipe = builtins.unsafeDiscardStringContext inventory.drvPath;
  fakeNix = pkgs.writeShellScriptBin "nix" ''
    set -euo pipefail
    case "$1" in
      flake) cat "$source_identity" ;;
      --version) echo fixture-nix ;;
      config) printf '{"pure-eval":%s}\n' "$(cat "$purity")" ;;
      eval) [[ ! -f "$TMPDIR/fail-evaluation" ]]; printf '%s\n' '${recipe}' ;;
      build)
        printf 'build\n' >> "$builds"
        previous=""
        for arg in "$@"; do
          if [[ "$previous" == --out-link ]]; then ln -sf ${inventory} "$arg"; fi
          previous="$arg"
        done
        printf '%s\n' ${inventory}
        ;;
      *) exit 64 ;;
    esac
  '';
  store = pkgs.writeShellScriptBin "nix-store" ''
    set -euo pipefail
    case "$1" in
      --check-validity)
        [[ "$2" == '${recipe}' || "$2" == ${inventory} ]]
        [[ ! -f "$invalid" ]]
        ;;
      --add-root)
        [[ "''${!#}" == ${inventory} ]]
        ln -sf ${inventory} "$2"
        ;;
      *) exit 64 ;;
    esac
  '';
in
pkgs.runCommand "verification-inventory-cache-conformance"
  {
    nativeBuildInputs = [
      pkgs.bash
      pkgs.coreutils
      pkgs.jq
      fakeNix
      store
    ];
  }
  ''
    set -euo pipefail
    export source_identity="$TMPDIR/source" builds="$TMPDIR/builds" purity="$TMPDIR/purity" invalid="$TMPDIR/invalid"
    export XDG_CACHE_HOME="$TMPDIR/cache"
    printf '%s\n' true > "$purity"
    printf '%s\n' '{"locked":{"rev":"fixture","narHash":"first"},"url":"fixture-reference","locks":{"nodes":{"root":{}}},"path":"/nix/store/fixture-source"}' > "$source_identity"
    mkdir "$TMPDIR/work"
    source ${./verification-inventory.sh}
    first=$(verification_inventory fixture unit "$TMPDIR/work")
    second=$(verification_inventory fixture unit "$TMPDIR/work")
    test "$first" = ${inventory}
    test "$first" = "$second"
    test "$(wc -l < "$builds")" = 1
    printf '%s\n' false > "$purity"
    test "$(verification_inventory fixture unit "$TMPDIR/work")" = ${inventory}
    test "$(wc -l < "$builds")" = 2
    printf '%s\n' '{"locked":{"rev":"fixture","narHash":"second"},"url":"fixture-reference","locks":{"nodes":{"root":{}}},"path":"/nix/store/fixture-source"}' > "$source_identity"
    test "$(verification_inventory fixture unit "$TMPDIR/work")" = ${inventory}
    test "$(wc -l < "$builds")" = 3
    touch "$invalid"
    test "$(verification_inventory fixture unit "$TMPDIR/work")" = ${inventory}
    test "$(wc -l < "$builds")" = 4
    rm "$invalid"
    printf '%s\n' '{"locked":{"rev":"fixture","narHash":"second"},"url":"fixture-reference","locks":{"nodes":{"root":{},"mutable":{"locked":{"type":"path"}}}},"path":"/nix/store/fixture-source"}' > "$source_identity"
    verification_inventory fixture unit "$TMPDIR/work" > /dev/null
    verification_inventory fixture unit "$TMPDIR/work" > /dev/null
    test "$(wc -l < "$builds")" = 6
    touch "$TMPDIR/fail-evaluation"
    if verification_inventory fixture unit "$TMPDIR/work" > /dev/null; then
      echo 'failed evaluation must not return cached metadata' >&2
      exit 1
    fi
    test "$(wc -l < "$builds")" = 6
    touch "$out"
  ''
