# wrapix-agent-<agent>-<name>: tier 2 of the provenance-tiered fromImage chain
# (specs/image-builder.md § Provenance-Tiered Layering). Holds exactly the one
# selected `agentPkg` runtime and its closure; a non-selected agent's binary is
# absent. Chained atop wrapix-stable-profile-<name>, with the
# per-profile leaf chaining on top via fromImage, so the agent binary loads into
# the platform store once and an agent-version bump re-emits only this tier and
# the leaf, leaving the heavier toolchain tier (below) byte-identical.
#
# Built with buildLayeredImage (a tar in the store) on every platform so it is
# usable as the fromImage of the leaf on both Linux and Darwin, matching the
# tar-only constraint base-image.nix records for tier 0.
{
  pkgs,
  agentPackages,
  agentImageName,
  stableProfileImage,
}:

let
  inherit (pkgs) dockerTools;

  # The selected agent runtime, laid down as its own real store path(s) — not
  # wrapped in a buildEnv, whose merged symlink tree dockerTools copies to the
  # image root without materializing the wrapper's own store path, which would
  # then register as a dangling path in the leaf's Nix DB
  # (specs/image-builder.md § In-Container Nix Store Consistency). Exactly one
  # agent rides here. The leaf's profileEnv re-references the same store paths
  # and strips them via remove_paths, resolving them from this layer at runtime.
  tierContents = agentPackages;

  # Roots tiers 0+1+2 contribute to the composed image: everything the stable
  # profile (and base below it) ship plus this tier's own contents. Exposed so
  # the leaf can remove_paths the whole union from its layering graph
  # (specs/image-builder.md § Provenance-Tiered Layering).
  lowerTiersRootPaths = stableProfileImage.lowerTiersRootPaths ++ tierContents;

  # The store paths tiers 0+1+2 actually MATERIALIZE on disk: the stable
  # profile's materialized contents plus this tier's `contents`. Exposed so the
  # leaf registers the baked Nix DB over the materialized contents closure
  # (specs/image-builder.md § In-Container Nix Store Consistency).
  lowerTiersContents = stableProfileImage.lowerTiersContents ++ tierContents;

  # Everything tiers 0+1+2 ship, exposed so the leaf's custom layeringPipeline
  # can remove_paths the whole union. The custom pipeline does not dedup
  # fromImage, so the leaf must strip this explicitly.
  lowerTiersClosure = pkgs.closureInfo {
    rootPaths = lowerTiersRootPaths;
  };

  # Bounded to this tier's own agent closure; the 127-layer OCI ceiling splits
  # across base (64) + stable-profile (40) + agent (8) + leaf (15).
  maxLayers = 8;

  # Split only this tier's own paths into layers. dockerMakeLayers (the custom
  # layeringPipeline path) does not dedup fromImage the way the default
  # popularity-contest path does, so remove_paths strips the lower tiers'
  # (base + stable-profile) closure first — a path a lower tier already ships is
  # never re-emitted here.
  layeringPipeline =
    pkgs.runCommandLocal "${agentImageName}-layering.json"
      {
        nativeBuildInputs = [ pkgs.jq ];
        lowerClosure = stableProfileImage.lowerTiersClosure;
      }
      ''
        set -euo pipefail
        jq -n \
          --rawfile storePaths "$lowerClosure/store-paths" \
          --argjson maxLayers ${toString maxLayers} \
          '($storePaths | split("\n") | map(select(length > 0))) as $lower
           | [
               [ "remove_paths", $lower ],
               [ "popularity_contest" ],
               [ "limit_layers", $maxLayers ]
             ]' \
          > "$out"
      '';
in
dockerTools.buildLayeredImage {
  name = agentImageName;
  tag = "latest";
  fromImage = stableProfileImage;
  inherit layeringPipeline;

  contents = tierContents;
}
// {
  inherit
    lowerTiersClosure
    lowerTiersRootPaths
    lowerTiersContents
    stableProfileImage
    ;
}
