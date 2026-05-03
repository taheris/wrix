{
  pkgs,
  fenix ? null,
}:

let
  fenixPkgs =
    if fenix == null then
      throw "lib/loom/default.nix: requires the fenix input; pass `fenix` to this file"
    else
      fenix.packages.${pkgs.stdenv.hostPlatform.system};

  toolchain = fenixPkgs.stable.defaultToolchain;

  rustPlatform = pkgs.makeRustPlatform {
    cargo = toolchain;
    rustc = toolchain;
  };
in
rustPlatform.buildRustPackage {
  pname = "loom";
  version = "0.1.0";
  src = ../../loom;

  cargoLock = {
    lockFile = ../../loom/Cargo.lock;
  };

  env.HOME = "/tmp";

  doCheck = true;
  nativeCheckInputs = [ pkgs.git ];

  meta = {
    description = "Rust agent driver for the Wrapix workflow (replaces Ralph's bash scripts)";
    mainProgram = "loom";
  };
}
