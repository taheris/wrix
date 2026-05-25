# Profile-image manifest helper.
#
# Builds a `profile-images.json` derivation mapping each profile name to the
# podman ref and Nix store path needed to spawn its image. Orchestrators
# (e.g. Loom via `LOOM_PROFILES_MANIFEST`) read this at startup and use each
# entry's `ref`/`source` to populate `SpawnConfig.image_ref`/`image_source`
# per bead.
#
# See specs/profiles.md § Profile-Image Manifest.
{
  pkgs,
}:

let
  imageTagLib = import ../util/image-tag.nix { };

  # podman accepts `localhost/<name>:<tag>` refs; Apple's `container` CLI
  # rejects the `localhost/` prefix and uses bare `<name>:<tag>`. Match the
  # platform convention used by the corresponding launcher (linux/darwin
  # sandbox default.nix).
  refPrefix = if pkgs.stdenv.isDarwin then "" else "localhost/";

  mkEntry = image: {
    ref = "${refPrefix}${image.imageName}:${imageTagLib.mkImageTag image}";
    source = "${image}";
  };
in
{
  mkProfileImages =
    images:
    let
      manifest = builtins.mapAttrs (_name: mkEntry) images;
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
