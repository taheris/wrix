{ pkgs, rustProfile }:

let
  workspace = rustProfile.buildPackage {
    src = ../..;
    cargoLock = ../../Cargo.lock;

    meta = {
      description = "Rust wrix service and cache CLI foundation";
      mainProgram = "wrix";
    };
  };

  mkBinaryPackage =
    name:
    pkgs.runCommand name
      {
        meta = {
          description = "Rust ${name} binary";
          mainProgram = name;
        };
      }
      ''
        mkdir -p "$out/bin"
        ln -s "${workspace.bin}/bin/${name}" "$out/bin/${name}"
      '';

in
{
  inherit (workspace) cargoArtifacts clippy nextest;

  package = workspace.bin;
  wrix = mkBinaryPackage "wrix";
  cacheHook = mkBinaryPackage "wrix-cache-hook";
  cachePublish = mkBinaryPackage "wrix-cache-publish";
  cacheServe = mkBinaryPackage "wrix-cache-serve";
}
