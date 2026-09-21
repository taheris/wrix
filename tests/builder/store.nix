{ pkgs }:

let
  image = import ../../lib/sandbox/builder/image.nix { inherit pkgs; };
  importShell =
    if pkgs.stdenv.hostPlatform.isLinux then
      "${pkgs.pkgsStatic.busybox}/bin/busybox sh"
    else
      "${pkgs.bash}/bin/bash";
  accountRoots = builtins.filter (
    root:
    builtins.elem root.name [
      "passwd"
      "group"
    ]
  ) image.darwin_seed_roots;
  accountClosure =
    assert builtins.length accountRoots == 2;
    pkgs.writeText "builder-account-closure" (
      pkgs.lib.concatMapStrings (root: "${root}\n") accountRoots
    );
  runtimeRoot = import ../../lib/builder/runtime-root.nix {
    inherit pkgs;
    seedRoots = [ accountClosure ];
  };
  updatedRoot = import ../../lib/builder/runtime-root.nix {
    inherit pkgs;
    seedRoots = [
      accountClosure
      (pkgs.writeText "builder-runtime-update" "updated\n")
    ];
  };
  exportRuntime =
    root:
    import ../../lib/builder/darwin-store-export.nix {
      inherit pkgs;
      seedRoots = [ root ];
    };
  registration = pkgs.closureInfo {
    rootPaths = [
      runtimeRoot
      updatedRoot
    ];
  };
in
pkgs.runCommandLocal "test-builder-store-gc-recovery"
  {
    nativeBuildInputs = [
      pkgs.bash
      pkgs.coreutils
      pkgs.gnugrep
      pkgs.nix
    ];
  }
  ''
    set -euo pipefail
    export HOME="$TMPDIR/home"
    mkdir -p "$HOME"
    export NIX_CONFIG="build-users-group ="
    export NIX_REMOTE=local
    export NIX_LOG_DIR="$TMPDIR/nix-log"
    source_state="$TMPDIR/source-state"
    target="$TMPDIR/persistent-root"

    NIX_STATE_DIR="$source_state" nix-store --load-db < ${registration}/registration
    export_runtime() {
      NIX_STATE_DIR="$source_state" "$1"
    }
    collect() {
      if ! nix-store --store "$target" --gc >"$TMPDIR/gc.log" 2>&1; then
        cat "$TMPDIR/gc.log" >&2
        exit 1
      fi
    }

    passwd_path=""
    while IFS= read -r root; do
      if [[ -f "$root/etc/passwd" ]]; then
        passwd_path="$root/etc/passwd"
        break
      fi
    done < ${accountClosure}
    [[ -n "$passwd_path" ]]

    export_runtime ${exportRuntime runtimeRoot} | nix-store --store "$target" --import >/dev/null
    [[ -f "$target$passwd_path" ]]
    collect
    [[ ! -e "$target$passwd_path" ]]
    echo "Reproduced: GC removes the unrooted image passwd file."

    printf 'existing project build\n' >"$TMPDIR/project"
    project=$(nix-store --store "$target" --add "$TMPDIR/project")
    ln -s "$project" "$target/nix/var/nix/gcroots/project"
    printf 'collectable garbage\n' >"$TMPDIR/garbage"
    garbage=$(nix-store --store "$target" --add "$TMPDIR/garbage")

    export_runtime ${exportRuntime runtimeRoot} \
      | ${importShell} ${../../lib/builder/import-store.sh} "$target" ${runtimeRoot}
    grep -q '^builder:x:1000:' "$target$passwd_path"
    cmp "$TMPDIR/project" "$target$project"
    [[ "$(readlink "$target/nix/var/nix/gcroots/wrix-builder-runtime")" == ${runtimeRoot} ]]
    collect
    [[ -f "$target$passwd_path" ]]
    [[ ! -e "$target$garbage" ]]
    cmp "$TMPDIR/project" "$target$project"
    nix-store --store "$target" --verify --check-contents
    echo "Restored runtime survives GC; unrelated project data is preserved."

    export_runtime ${exportRuntime updatedRoot} \
      | ${importShell} ${../../lib/builder/import-store.sh} "$target" ${updatedRoot}
    collect
    [[ ! -e "$target"${runtimeRoot} ]]
    [[ -f "$target$passwd_path" ]]
    cmp "$TMPDIR/project" "$target$project"
    [[ "$(readlink "$target/nix/var/nix/gcroots/wrix-builder-runtime")" == ${updatedRoot} ]]
    nix-store --store "$target" --verify --check-contents
    echo "Runtime update replaces its GC root without resetting the store."
    touch "$out"
  ''
