{ pkgs }:
let
  imageTools = import ../../lib/sandbox/image-tools.nix { inherit pkgs; };
  runtimeClosure = pkgs.closureInfo { rootPaths = [ imageTools ]; };
  payload = pkgs.writeText "image-tool-native-root" "opaque native input";
  danglingSource = pkgs.runCommand "image-tool-native-source-symlink" { } ''
    ln -s /not-a-real-wrix-source-target "$out"
  '';
  references = pkgs.writeText "image-tool-references.json" (
    builtins.toJSON [ "${payload}?annotation" ]
  );
  sources = pkgs.writeText "image-tool-sources.json" (builtins.toJSON [ danglingSource ]);
in
pkgs.runCommand "image-tools-native-contract"
  {
    nativeBuildInputs = [
      imageTools
      pkgs.coreutils
    ];
  }
  ''
    set -euo pipefail
    while IFS= read -r path; do
      case "$path" in
        *-python[0-9]*|*-celler-*|*-wrix-host-controller-*)
          echo "unexpected native image-tool runtime dependency: $path" >&2
          exit 1
          ;;
      esac
    done < ${runtimeClosure}/store-paths
    # Independently hash token bytes, including escapes and significant spaces.
    printf '%s' '{"z":-0,"a":1e+00,"s":"two  words\\\" λ"}' > compact.json
    printf ' \n%s\t\n' "$(cat compact.json)" > pretty.json
    expected="sha256:$(sha256sum compact.json | cut -d' ' -f1)"
    actual=$(wrix-image-tools config-digest < pretty.json)
    test "$actual" = "$expected"
    if printf '%s' '{"broken":' | wrix-image-tools config-digest > invalid.out 2> invalid.err; then
      echo 'invalid JSON was accepted' >&2
      exit 1
    fi
    test ! -s invalid.out

    printf '%s\n' ${payload} > closure
    wrix-image-tools configuration-roots ${references} closure resolved ${sources}
    test "$(readlink resolved/share/wrix/image-inputs/${builtins.baseNameOf payload})" = ${payload}
    # lstat, not canonicalize/stat: an opaque source symlink is itself the input.
    test -L resolved/share/wrix/image-inputs/${builtins.baseNameOf danglingSource}
    test "$(readlink resolved/share/wrix/image-inputs/${builtins.baseNameOf danglingSource})" = ${danglingSource}
    test ! -e ${danglingSource}

    : > empty-closure
    if wrix-image-tools configuration-roots ${references} empty-closure denied ${sources} > denied.out 2> denied.err; then
      echo 'out-of-closure token was accepted' >&2
      exit 1
    fi
    test ! -e denied
    touch "$out"
  ''
