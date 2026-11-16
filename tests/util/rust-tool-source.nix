{ pkgs }:
let
  source =
    package:
    import ../../lib/util/rust-tool-source.nix {
      inherit pkgs package;
      root = ../..;
    };
  fixture =
    name: minimalUnusedTargets:
    import ../../lib/util/rust-tool-source.nix {
      inherit pkgs minimalUnusedTargets;
      package = "native-tool";
      root = ./rust-tool-targets + "/${name}";
    };
  imageSource = (import ../../lib/sandbox/image-tools.nix { inherit pkgs; }).src;
  environmentSource =
    (import ../../lib/sandbox/runtime-environment.nix { inherit pkgs; }).package.src;
  expectedSource =
    package: extraSources:
    import ../../lib/util/rust-tool-source.nix {
      inherit pkgs package extraSources;
      root = ../..;
      minimalUnusedTargets = true;
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
assert
  toString imageSource
  == toString (expectedSource "wrix-image-tools" [ ../../lib/sandbox/oci-layout-build.sh ]);
assert toString environmentSource == toString (expectedSource "wrix-runtime-env" [ ]);
assert (fixture "base" true).drvPath == (fixture "added" true).drvPath;
assert (fixture "base" false).drvPath != (fixture "added" false).drvPath;
pkgs.runCommand "rust-tool-source"
  {
    nativeBuildInputs = [
      pkgs.cargo
      pkgs.rustc
      pkgs.stdenv.cc
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
    cp -r ${fixture "added" true}/. "$TMPDIR/tool"
    chmod -R u+w "$TMPDIR/tool"
    cargo test --offline --locked --manifest-path "$TMPDIR/tool/Cargo.toml" -p native-tool
    test "$("$TMPDIR/tool/target/debug/native-tool")" = 7
    cp -r ${./rust-tool-targets/added}/. "$TMPDIR/complete"
    chmod -R u+w "$TMPDIR/complete"
    if cargo test --offline --locked --manifest-path "$TMPDIR/complete/Cargo.toml" --workspace > "$TMPDIR/full.log" 2>&1; then
      echo 'full workspace must still execute the added failing integration test' >&2
      exit 1
    fi
    grep -F 'full workspace sees added test' "$TMPDIR/full.log"
    touch "$out"
  ''
