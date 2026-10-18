{
  pkgs,
  controller,
  runtime,
  image,
}:

pkgs.writeText "wrix-controller-binding.json" (
  builtins.toJSON {
    controller = toString controller;
    runtime = toString runtime;
    image = builtins.unsafeDiscardOutputDependency image.source.drvPath;
  }
)
