{ upstream }:

arguments@{ pkgs, wrixLib, ... }:
let
  inherit (builtins)
    attrValues
    concatLists
    listToAttrs
    mapAttrs
    toJSON
    unsafeDiscardStringContext
    ;
  original = upstream.mkProfileManifest arguments;
  inherit (original) sandboxes;
  manifest = mapAttrs (
    _profile: byRuntime:
    mapAttrs (runtime: entry: entry.${runtime})
      (wrixLib.mkProfileImages (mapAttrs (_runtime: sandbox: sandbox.image) byRuntime)).manifest
  ) sandboxes;
  nativeCapabilities = listToAttrs (
    concatLists (
      attrValues (
        mapAttrs (
          profile: byRuntime:
          if profile == "rust" then
            map (sandbox: {
              name = unsafeDiscardStringContext (toString sandbox.image.profileConfig);
              value = "rust";
            }) (attrValues byRuntime)
          else
            [ ]
        ) sandboxes
      )
    )
  );
in
pkgs.runCommand "profile-images.json"
  {
    outputs = [
      "out"
      "nativePreflight"
    ];
    passthru = {
      inherit (original) profiles runtimes;
      inherit manifest sandboxes nativeCapabilities;
    };
  }
  ''
    set -euo pipefail
    cp ${pkgs.writeText "images.json" (toJSON manifest)} "$out"
    cp ${pkgs.writeText "native-preflight.json" (toJSON nativeCapabilities)} "$nativePreflight"
  ''
