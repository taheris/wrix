{ pkgs }:
import ../util/rust-tool.nix {
  inherit pkgs;
  package = "wrix-image-tools";
  description = "Native OCI configuration and image input tooling";
  nativeCheckInputs = [ pkgs.gnutar ];
}
