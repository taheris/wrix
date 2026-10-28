{ pkgs }:

let
  libkrun = pkgs.libkrun.override { withNet = true; };
  runtime = import ../../lib/sandbox/linux/krun-runtime.nix { linuxPkgs = pkgs; };
  closure = pkgs.closureInfo { rootPaths = [ runtime ]; };
in
assert libkrun.src == pkgs.libkrun.src;
assert (libkrun.patches or [ ]) == (pkgs.libkrun.patches or [ ]);
pkgs.runCommand "krun-passt-package-binding"
  {
    nativeBuildInputs = [ pkgs.python3 ];
  }
  ''
    set -euo pipefail
    ${runtime}/bin/crun --version
    python3 - <<'PY'
    import ctypes
    from pathlib import Path

    runtime = Path("${runtime}/bin/crun").read_bytes()
    library = "${libkrun}/lib/libkrun.so.1"
    helper = "${pkgs.passt}/bin/passt"
    paths = Path("${closure}/store-paths").read_text().splitlines()
    assert "${libkrun}" in paths
    assert "${pkgs.passt}" in paths
    assert "${pkgs.libkrun}" not in paths
    assert (library + "\0").encode() in runtime
    assert (helper + "\0").encode() in runtime
    assert b"/usr/bin/passt\0" not in runtime
    assert ("${pkgs.libkrun}/lib/libkrun.so.1\0").encode() not in runtime
    assert callable(ctypes.CDLL(library).krun_add_net_unixstream)
    PY
    touch "$out"
  ''
