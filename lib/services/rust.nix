{ pkgs, rustProfile }:

let
  inherit (builtins) concatStringsSep;

  workspace = rustProfile.buildPackage {
    src = ../..;
    cargoLock = ../../Cargo.lock;
    nativeBuildInputs = [ pkgs.git ];

    meta = {
      description = "Rust wrix service and cache CLI foundation";
      mainProgram = "wrix";
    };
  };

  prekHooksBundle = import ../prek/bundle.nix { inherit pkgs; };

  binaryMeta = name: {
    description = "Rust ${name} binary";
    mainProgram = name;
  };

  mkBinaryPackage =
    name:
    pkgs.runCommand name
      {
        meta = binaryMeta name;
      }
      ''
        mkdir -p "$out/bin"
        ln -s "${workspace.bin}/bin/${name}" "$out/bin/${name}"
      '';

  mkWrappedBinaryPackage =
    name: wrapperArgs:
    pkgs.runCommand name
      {
        nativeBuildInputs = [ pkgs.makeWrapper ];
        meta = binaryMeta name;
      }
      ''
        mkdir -p "$out/bin"
        makeWrapper "${workspace.bin}/bin/${name}" "$out/bin/${name}" ${concatStringsSep " " wrapperArgs}
        ln -s "${workspace.bin}/bin/wrix-git-sign" "$out/bin/wrix-git-sign"
      '';

in
{
  inherit (workspace) cargoArtifacts clippy nextest;

  package = workspace.bin;
  wrix = mkWrappedBinaryPackage "wrix" [
    "--set"
    "WRIX_PREK_HOOKS"
    "${prekHooksBundle}"
  ];
  cacheHook = mkBinaryPackage "wrix-cache-hook";
  cachePublish = mkBinaryPackage "wrix-cache-publish";
  cacheServe = mkBinaryPackage "wrix-cache-serve";
}
