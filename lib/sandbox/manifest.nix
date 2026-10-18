# Profile-image manifest helper.
#
# Builds a `profile-images.json` derivation mapping each profile name to one
# agent variant (`direct`, `claude`, or `pi`) with the image ref, image source,
# source kind and profile config needed to spawn that variant. Orchestrators (e.g. Loom via
# `LOOM_PROFILES_MANIFEST`) read this at startup and use the selected variant to
# populate `wrix spawn` inputs per bead.
#
# See specs/profiles.md § Profile-Image Manifest.
{ pkgs }:

let
  inherit (builtins) elem mapAttrs toString;

  mkImageEntry =
    image:
    let
      sourceKind = image.source_kind or (throw "mkProfileImages: image.source_kind is required");
      source = image.source or image;
    in
    {
      inherit (image) ref;
      source = toString source;
      source_kind = sourceKind;
      profile_config = toString (
        image.profileConfig or (throw "mkProfileImages: image.profileConfig is required")
      );
    };

  mkAgentEntry =
    image:
    let
      agent = image.agent or "direct";
      agents = [
        "direct"
        "claude"
        "pi"
      ];

    in
    if elem agent agents then
      { ${agent} = mkImageEntry image; }
    else
      throw "mkProfileImages: image.agent must be one of direct, claude, or pi";
in
{
  mkProfileImages =
    images:
    let
      manifest = mapAttrs (_name: mkAgentEntry) images;
    in
    pkgs.writeTextFile {
      name = "profile-images.json";
      text = builtins.toJSON manifest;
      passthru = {
        # Eval-time access for tests / consumers that want the structured
        # data without realizing the writeText derivation (which would
        # transitively build every referenced image).
        inherit manifest;
      };
    };
}
