{ pkgs, src }:
(import ../../lib/verify/tools.nix { inherit pkgs; }).overrideAttrs (old: {
  WRIX_EXECUTION_SOURCE = toString src;
  postInstall = (old.postInstall or "") + ''
    mkdir -p "$out/libexec"
    found=0
    for binary in target/*/release/deps/execution_lifecycle-*; do
      if [[ -f "$binary" && -x "$binary" ]]; then
        test "$found" = 0
        cp "$binary" "$out/libexec/execution-lifecycle"
        found=1
      fi
    done
    test "$found" = 1
  '';
})
