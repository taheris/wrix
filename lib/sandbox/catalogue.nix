{ pkgs, mkProfileImages }:

images:
let
  inherit (builtins)
    elem
    getContext
    isString
    mapAttrs
    toJSON
    toString
    unsafeDiscardOutputDependency
    unsafeDiscardStringContext
    ;
  inherit (pkgs.lib) isDerivation;
  checkedImage =
    agent: image:
    if
      !(elem agent [
        "direct"
        "claude"
        "pi"
      ])
      || (image.agent or "direct") != agent
    then
      throw "mkProfileCatalogue: runtime key must match the image's selected agent"
    else if
      !(elem (image.source_kind or null) [
        "nix-descriptor"
        "docker-archive"
      ])
    then
      throw "mkProfileCatalogue: unsupported image.source_kind"
    else if !(isString (image.ref or null)) || image.ref == "" then
      throw "mkProfileCatalogue: image.ref must be nonempty"
    else if !(isDerivation (image.source or null)) || !(isDerivation (image.profileConfig or null)) then
      throw "mkProfileCatalogue: image.source and image.profileConfig must be pinned derivations"
    else
      image;
  provisions = mapAttrs (
    profile: variants:
    mapAttrs (
      agent: image:
      (mkProfileImages { ${profile} = checkedImage agent image; }).overrideAttrs {
        allowSubstitutes = true;
      }
    ) variants
  ) images;
  profiles = mapAttrs (
    profile: variants:
    mapAttrs (
      agent: image:
      let
        selected = checkedImage agent image;
        provision = provisions.${profile}.${agent};
      in
      {
        inherit (selected) ref source_kind;
        realization = {
          path = unsafeDiscardStringContext (toString provision);
          derivation = unsafeDiscardOutputDependency provision.drvPath;
        };
      }
    ) variants
  ) images;
  catalogue = {
    schema = 1;
    inherit profiles;
  };
  text = toJSON catalogue;
in
if builtins.all (context: context == { path = true; }) (builtins.attrValues (getContext text)) then
  pkgs.writeTextFile {
    name = "profile-catalogue.json";
    inherit text;
    passthru = { inherit catalogue provisions; };
  }
else
  throw "mkProfileCatalogue: catalogue metadata must not demand derivation outputs"
