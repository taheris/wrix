# Bind both dockerTools planner paths to genuine native implementations. Keep
# upstream archive/configuration construction and its verification unchanged.
{ pkgs }:
let
  native = pkgs.buildPackages;
  planner = import ./image-tools.nix { pkgs = native; };
  dockerMakeLayers = native.callPackage (native.path + "/pkgs/build-support/docker/make-layers.nix") {
    flatten-references-graph = planner;
  };
  dockerAutoLayer = import ./auto-layer.nix {
    pkgs = native;
    inherit planner;
  };
in
pkgs.dockerTools.override {
  buildPackages = native // {
    inherit dockerMakeLayers dockerAutoLayer;
  };
}
