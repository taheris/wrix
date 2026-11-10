{ pkgs, rustProfile }:
let
  probe = rustProfile.buildPackage {
    src = ../fixtures/cargo-dummy-gc;
    cargoLock = ../fixtures/cargo-dummy-gc/Cargo.lock;
  };
  source = probe.cargoArtifacts.src;
  closure = pkgs.closureInfo { rootPaths = [ source ]; };
in
pkgs.runCommand "cargo-dummy-source-collection"
  {
    nativeBuildInputs = [
      pkgs.nix
      pkgs.yq
    ];
    passthru = { inherit probe source closure; };
  }
  ''
    set -euo pipefail
    export HOME="$TMPDIR/home"
    mkdir -p "$HOME"
    script=$(tomlq -r '.package.build' ${source}/Cargo.toml)
    [[ "$script" == /nix/store/*-dummyBuild.rs ]]
    grep -Fx "$script" ${closure}/store-paths
    test -d ${probe.cargoArtifacts}
    private="$TMPDIR/private-store"
    mkdir -p "$private/nix/store"
    while IFS= read -r path; do
      [[ "$path" == /nix/store/* ]]
      nix-store --dump "$path" > "$TMPDIR/input.nar"
      nix-store --restore "$private$path" < "$TMPDIR/input.nar"
    done < ${closure}/store-paths
    nix-store --store "$private" --load-db < ${closure}/registration
    nix-store --store "$private" --option substituters "" \
      --add-root "$TMPDIR/source-root" --indirect --realise ${source}
    nix-store --store "$private" --gc
    nix-store --store "$private" --verify-path "$script"
    test -f "$private$script"
    nix-store --store "$private" --query --requisites ${source} | sort > actual
    sort ${closure}/store-paths > expected
    cmp expected actual
    rm "$TMPDIR/source-root"
    nix-store --store "$private" --gc
    if nix-store --store "$private" --check-validity "$script"; then
      echo 'unrooted private dummy input unexpectedly survived collection' >&2
      exit 1
    fi
    test ! -e "$private$script"
    mkdir "$out"
  ''
