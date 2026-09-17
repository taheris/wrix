{
  pkgs,
  linuxPkgs ? pkgs,
}:

let
  fixtureArchive = pkgs.writeText "wrix-builder-fixture-archive" "fixture\n";
  builderImage = {
    darwin_seed_roots = [ fixtureArchive ];
    digest = pkgs.writeText "wrix-builder-fixture-digest" ''
      sha256:0000000000000000000000000000000000000000000000000000000000000000
    '';
    ref = "wrix-builder:fixture";
    source = fixtureArchive;
    source_kind = "docker-archive";
  };
in
import ../../lib/builder {
  inherit pkgs linuxPkgs builderImage;
}
