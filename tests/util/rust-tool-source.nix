{ pkgs }:
let
  source =
    package:
    import ../../lib/util/rust-tool-source.nix {
      inherit pkgs package;
      root = ../..;
    };
  rejected =
    builtins.tryEval
      (import ../../lib/util/rust-tool-source.nix {
        inherit pkgs;
        root = ./rust-tool-local-dependency;
        package = "probe";
      }).drvPath;
in
assert !rejected.success;
pkgs.runCommand "rust-tool-source"
  {
    nativeBuildInputs = [
      pkgs.cargo
      pkgs.jq
    ];
  }
  ''
    set -euo pipefail
    export HOME="$TMPDIR/home"
    mkdir -p "$HOME"
    for package in wrix-notify wrix-image-tools wrix-verify-tools wrix-runtime-env; do
      case "$package" in
        wrix-notify) source=${source "wrix-notify"} ;;
        wrix-image-tools) source=${source "wrix-image-tools"} ;;
        wrix-verify-tools) source=${source "wrix-verify-tools"} ;;
        wrix-runtime-env) source=${source "wrix-runtime-env"} ;;
      esac
      cargo metadata --offline --locked --no-deps --format-version 1 \
        --manifest-path "$source/Cargo.toml" > "$TMPDIR/metadata.json"
      jq -e --arg package "$package" '.packages | any(.name == $package)' "$TMPDIR/metadata.json"
      diff -r "${../../crates}/$package" "$source/crates/$package"
      for member in "$source"/crates/*; do
        name="$(basename "$member")"
        cmp "${../../crates}/$name/Cargo.toml" "$member/Cargo.toml"
        if [[ "$name" != "$package" ]]; then
          test ! -s "$member/src/lib.rs"
          find "$member" -type f ! -name Cargo.toml ! -path '*/src/lib.rs' > "$TMPDIR/unexpected"
          test ! -s "$TMPDIR/unexpected"
        fi
      done
    done
    touch "$out"
  ''
