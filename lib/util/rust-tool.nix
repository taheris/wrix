# Compile one native workspace tool, retaining canonical manifests and lock.
{
  pkgs,
  package,
  description,
  extraSources ? [ ],
  nativeCheckInputs ? [ ],
}:
let
  inherit (pkgs) lib;
  workspace = builtins.fromTOML (builtins.readFile ../../Cargo.toml);
in
pkgs.rustPlatform.buildRustPackage {
  pname = package;
  inherit (workspace.workspace.package) version;
  src = lib.fileset.toSource {
    root = ../..;
    fileset = lib.fileset.unions (
      [
        ../../Cargo.toml
        ../../Cargo.lock
        ../../clippy.toml
        # Other members retain their real manifests and target entry points for
        # Cargo metadata, without adding unrelated implementation source inputs.
        (lib.fileset.fileFilter (
          file:
          builtins.elem file.name [
            "Cargo.toml"
            "lib.rs"
            "main.rs"
          ]
        ) ../../crates)
        (../../crates + "/${package}")
      ]
      ++ extraSources
    );
  };
  cargoLock = {
    lockFile = ../../Cargo.lock;
    outputHashes = {
      "attic-0.1.0" = "sha256-qDnTXcnV7ZvmGMtpEse+ymr05QuuULtlzroqGrpP8VA=";
      "nix-daemon-0.1.1" = "sha256-Jd4E7qN/9pCCYBLxWwHKjrRBt7gTBEZUYzoCAGH1nXg=";
    };
  };
  nativeBuildInputs = [ pkgs.bash ];
  inherit nativeCheckInputs;
  cargoBuildFlags = [
    "--package"
    package
  ];
  cargoTestFlags = [
    "--package"
    package
  ];
  doCheck = true;
  meta = {
    inherit description;
    license = lib.licenses.mit;
    mainProgram = package;
  };
}
