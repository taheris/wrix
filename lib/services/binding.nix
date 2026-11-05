{ pkgs, image }:

let
  artifact = path: derivation: {
    path = builtins.unsafeDiscardStringContext (toString path);
    derivation = builtins.unsafeDiscardOutputDependency derivation.drvPath;
  };
  resources = image.resources or null;
in
pkgs.writeText "wrix-service-binding.json" (
  builtins.toJSON (
    {
      schema = if resources == null then 1 else 2;
      image = image.ref;
      inherit (image) source_kind;
      source = artifact image.source image.source;
      digest = artifact image.digest image.digestDerivation;
    }
    // pkgs.lib.optionalAttrs (resources != null) { inherit resources; }
  )
)
