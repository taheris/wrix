{ pkgs, rustPackage }:
let
  fixture = "crates/wrix-image-tools/tests/fixtures/oci-pax-header.bin";
  original = ../../crates/wrix-image-tools/tests/fixtures/oci-pax-header.bin;
in
pkgs.runCommand "rust-image-tool-fixture-projection" { } ''
  cmp ${rustPackage.nextest.src}/${fixture} ${original}
  cmp ${rustPackage.clippy.src}/${fixture} ${original}
  cmp ${rustPackage.cacheBackendTests.src}/${fixture} ${original}
  test ! -e ${rustPackage.package.src}/${fixture}
  test ! -e ${rustPackage.clientPackage.src}/${fixture}
  touch "$out"
''
