{
  pkgs,
  configuration,
  tools,
}:
pkgs.runCommandLocal "oci-layout-private-gc-controls"
  {
    nativeBuildInputs = [
      pkgs.nix
      pkgs.jq
      pkgs.coreutils
    ];
  }
  ''
    set -euo pipefail
    mkdir "$out"
    export HOME="$TMPDIR/home"
    mkdir "$HOME"
    original_custom=$(jq -er '.customisation_layer' ${configuration})
    for engine in original fixed; do
      root="$TMPDIR/$engine"
      store="$root/store"
      mkdir -p "$root/custom" "$root/build" "$store/nix/store"
      cp "$original_custom/layer.tar" "$root/custom/layer.tar"
      mkfifo "$root/custom/checksum"
      jq --arg custom "$root/custom" '.customisation_layer=$custom' ${configuration} > "$root/conf.json"
      nix-store --store "$store" --init
      destination="$store/nix/store/00000000000000000000000000000000-layout"
      if [[ "$engine" == original ]]; then
        ${tools}/bin/wrix-image-tools oci-layout "$root/conf.json" "$destination" > "$out/$engine-producer.log" 2>&1 &
      else
        out="$destination" NIX_BUILD_TOP="$root/build" bash ${../../lib/sandbox/oci-layout-build.sh} \
          ${tools}/bin/wrix-image-tools "$root/conf.json" > "$out/$engine-producer.log" 2>&1 &
      fi
      producer=$!
      (
        exec 9> "$root/custom/checksum"
        touch "$root/ready"
        for attempt in $(seq 1 600); do
          if [[ -e "$root/release" ]]; then
            printf '%s' "$(cat "$original_custom/checksum")" >&9
            exit 0
          fi
          sleep 0.1
        done
        exit 1
      ) &
      writer=$!
      for attempt in $(seq 1 600); do
        if [[ -e "$root/ready" ]]; then break; fi
        kill -0 "$producer"
        sleep 0.1
      done
      test -e "$root/ready"
      if [[ "$engine" == original ]]; then
        stage=$(find "$store/nix/store" -maxdepth 1 -name '.wrix-oci-*')
      else
        stage=$(find "$root/build" -name '.wrix-oci-*')
        test -z "$(find "$store/nix/store" -maxdepth 1 -name '.wrix-oci-*')"
      fi
      test -d "$stage/blobs/sha256"
      nix-store --store "$store" --gc > "$out/$engine-gc.log" 2>&1
      if [[ "$engine" == original ]]; then test ! -e "$stage"; else test -d "$stage"; fi
      touch "$root/release"
      wait "$writer"
      if wait "$producer"; then status=0; else status=$?; fi
      printf '%s\n' "$status" > "$out/$engine.exit"
      if [[ "$engine" == original ]]; then
        test "$status" -ne 0
        test ! -e "$destination"
      else
        test "$status" -eq 0
        test -s "$destination/wrix/manifest.json"
        test -s "$destination/index.json"
        cp "$destination/wrix/manifest.json" "$out/fixed-manifest.json"
      fi
    done
  ''
