# Capture only contextual store tokens, without reading unrealised .drv files.
{ lib }:
inputs:
let
  references =
    value:
    if builtins.isString value || builtins.isPath value || lib.isDerivation value then
      let
        # A literal .drv path needs the recipe, not every transitive build
        # output. Ordinary selected-output dependencies remain intact.
        text = builtins.unsafeDiscardOutputDependency (builtins.toJSON value);
        plain = builtins.unsafeDiscardStringContext text;
        tokens = map builtins.head (
          builtins.filter builtins.isList (
            builtins.split "(/nix/store/[0-9abcdfghijklmnpqrsvwxyz]{32}-[-a-zA-Z0-9+._?=]+)" plain
          )
        );
      in
      if builtins.hasContext text then map (lib.strings.addContextFrom text) tokens else [ ]
    else if builtins.isAttrs value then
      lib.concatMap references (builtins.attrValues value)
    else if builtins.isList value then
      lib.concatMap references value
    else
      [ ];
in
lib.sort builtins.lessThan (lib.unique (references inputs))
