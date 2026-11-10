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
    mkdir -p "$HOME" "$TMPDIR/original"
    cp ${../../Cargo.toml} "$TMPDIR/original/Cargo.toml"
    cp ${../../Cargo.lock} "$TMPDIR/original/Cargo.lock"
    cp -r ${../../crates} "$TMPDIR/original/crates"
    cargo metadata --offline --locked --no-deps --format-version 1 \
      --manifest-path "$TMPDIR/original/Cargo.toml" > "$TMPDIR/original.json"
    projection='.packages | map({name, targets: [.targets[] | {name, kind}] | sort_by(.name)}) | sort_by(.name)'
    jq -S "$projection" "$TMPDIR/original.json" > "$TMPDIR/expected-targets.json"
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
      jq -S "$projection" "$TMPDIR/metadata.json" > "$TMPDIR/actual-targets.json"
      cmp "$TMPDIR/expected-targets.json" "$TMPDIR/actual-targets.json"
      diff -r "${../../crates}/$package" "$source/crates/$package"
      for member in "$source"/crates/*; do
        name="$(basename "$member")"
        cmp "${../../crates}/$name/Cargo.toml" "$member/Cargo.toml"
        if [[ "$name" != "$package" ]]; then
          find "$member" -type f ! -name Cargo.toml -size +0c > "$TMPDIR/implementations"
          if [[ -s "$TMPDIR/implementations" ]]; then
            printf 'Unused member implementation leaked: %s\n' "$member" >&2
            exit 1
          fi
          find "$member" -type f ! -name Cargo.toml ! -name '*.rs' > "$TMPDIR/unexpected"
          test ! -s "$TMPDIR/unexpected"
        fi
      done
    done
    touch "$out"
  ''
