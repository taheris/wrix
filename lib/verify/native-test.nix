{ pkgs }:
import ../util/rust-tool.nix {
  inherit pkgs;
  package = "wrix-native-test";
  scopedSource = true;
  description = "Disposable native test ownership and diagnostic probes";
  nativeCheckInputs = [
    pkgs.git
  ]
  ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
    pkgs.util-linux
    pkgs.openssl
  ];
}
