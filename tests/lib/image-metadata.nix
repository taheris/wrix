{ lib }:

let
  inherit (lib) hasPrefix hasSuffix;
in
{
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
