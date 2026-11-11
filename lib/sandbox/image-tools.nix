{ pkgs }:
import ../util/rust-tool.nix {
  inherit pkgs;
  package = "wrix-image-tools";
  description = "Native OCI configuration and image input tooling";
  extraSources = [ ./oci-layout-build.sh ];
  nativeCheckInputs = [ pkgs.gnutar ];
}
