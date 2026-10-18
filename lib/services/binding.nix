{ pkgs, image }:

let
  artifact = path: derivation: {
    path = builtins.unsafeDiscardStringContext (toString path);
    derivation = builtins.unsafeDiscardOutputDependency derivation.drvPath;
  };
in
pkgs.writeText "wrix-service-binding.json" (
  builtins.toJSON {
    schema = 1;
    image = image.ref;
    inherit (image) source_kind;
    source = artifact image.source image.source;
    digest = artifact image.digest image.digestDerivation;
  }
)
