{ pkgs, craneLib }:

let
  source = pkgs.fetchFromGitHub {
    owner = "celler-cache";
    repo = "celler";
    rev = "de7e34848770f3cd274a276e15d3c459fee86f99";
    hash = "sha256-qDnTXcnV7ZvmGMtpEse+ymr05QuuULtlzroqGrpP8VA=";
  };
in
(pkgs.callPackage "${source}/crane.nix" { inherit craneLib; }).celler
