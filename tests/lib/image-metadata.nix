{ lib }:

let
  inherit (lib) hasPrefix hasSuffix;
in
{
  referencesImage =
    sources: text:
    let
      context = builtins.getContext text;
      contextPaths = builtins.attrNames context;
      canMatch =
        source:
        !(builtins.isAttrs source && source ? drvAttrs && source.drvAttrs ? name)
        || builtins.any (path: hasSuffix "-${source.drvAttrs.name}.drv" path) contextPaths;
    in
    # A context key must have the producer's derivation name before its hash can
    # match. Avoid instantiating unrelated image graphs for shell-only contexts;
    # same-name producers still require the complete context intersection.
    context != { }
    && builtins.any (
      source:
      canMatch source && builtins.intersectAttrs (builtins.getContext (toString source)) context != { }
    ) sources;

  source =
    {
      expected_kind,
      source_kind,
      source,
    }:
    source_kind == expected_kind
    && hasPrefix "/nix/store/" (toString source)
    && (
      (source_kind == "docker-archive")
      || (source_kind == "nix-descriptor" && hasSuffix "-nix-descriptor.json" (toString source))
    );

  labels =
    {
      image,
      kind,
      profile ? "",
      agent ? "",
    }:
    let
      labels = image.labels or { };
    in
    (labels."wrix.managed" or null) == "true"
    && (labels."wrix.image.kind" or null) == kind
    && (profile == "" || (labels."wrix.profile.name" or null) == profile)
    && (agent == "" || (labels."wrix.agent.kind" or null) == agent);
}
