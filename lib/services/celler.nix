{ pkgs, craneLib }:

let
  upstream = pkgs.fetchFromGitHub {
    owner = "celler-cache";
    repo = "celler";
    rev = "de7e34848770f3cd274a276e15d3c459fee86f99";
    hash = "sha256-qDnTXcnV7ZvmGMtpEse+ymr05QuuULtlzroqGrpP8VA=";
  };
  source = pkgs.applyPatches {
    name = "celler-source";
    src = upstream;
    patches = [
      ./celler-ensure-key.patch
      ./celler-work-drain.patch
    ];
    postPatch = ''
      cp ${./celler-work-drain.rs} server/src/work_drain.rs
      cp ${./celler-work-drain-state.rs} server/src/work_drain_state.rs
      cp ${./celler-work-drain-transport.rs} server/src/work_drain_transport.rs
    '';
  };
in
(pkgs.callPackage "${source}/crane.nix" { inherit craneLib; }).celler
