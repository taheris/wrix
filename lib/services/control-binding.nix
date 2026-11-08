{
  pkgs,
  controller,
  runtime,
  management ? null,
  image,
}:

pkgs.writeText "wrix-controller-binding.json" (
  builtins.toJSON (
    {
      controller =
        if management == null then
          toString controller
        else
          builtins.unsafeDiscardStringContext (toString controller);
      runtime =
        if management == null then
          toString runtime
        else
          builtins.unsafeDiscardStringContext (toString runtime);
      image = builtins.unsafeDiscardOutputDependency image.source.drvPath;
    }
    // pkgs.lib.optionalAttrs (management != null) {
      controller_derivation = builtins.unsafeDiscardOutputDependency controller.drvPath;
      runtime_derivation = builtins.unsafeDiscardOutputDependency runtime.drvPath;
      management = toString management;
    }
  )
)
