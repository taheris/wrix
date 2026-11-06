# Native build-time tools: compile only the small image-tool member, not Wrix's
# controller/service binaries. Keep the canonical workspace manifests and lock.
{ pkgs }:
let
  inherit (pkgs) lib;
in
pkgs.rustPlatform.buildRustPackage {
  pname = "wrix-image-tools";
  version = "0.1.0";
  src = lib.fileset.toSource {
    root = ../..;
    fileset = lib.fileset.unions [
      ../../Cargo.toml
      ../../Cargo.lock
      ../../clippy.toml
      # Cargo reads the other members' real manifests and target entry points
      # for workspace metadata. Their implementations are not helper inputs.
      (lib.fileset.fileFilter (
        file:
        builtins.elem file.name [
          "Cargo.toml"
          "lib.rs"
          "main.rs"
        ]
      ) ../../crates)
      ../../crates/wrix-image-tools
    ];
  };
  cargoLock = {
    lockFile = ../../Cargo.lock;
    # These authenticate the two git sources in the canonical workspace lock;
    # they do not enable or compile those unrelated workspace dependencies.
    outputHashes = {
      "attic-0.1.0" = "sha256-qDnTXcnV7ZvmGMtpEse+ymr05QuuULtlzroqGrpP8VA=";
      "nix-daemon-0.1.1" = "sha256-Jd4E7qN/9pCCYBLxWwHKjrRBt7gTBEZUYzoCAGH1nXg=";
    };
  };
  cargoBuildFlags = [
    "--package"
    "wrix-image-tools"
  ];
  cargoTestFlags = [
    "--package"
    "wrix-image-tools"
  ];
  doCheck = true;
  meta = {
    description = "Native OCI configuration and image input tooling";
    license = lib.licenses.mit;
    mainProgram = "wrix-image-tools";
  };
}
