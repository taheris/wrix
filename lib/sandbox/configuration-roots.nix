# Keep runtime references without installing generated JSON or plaintext inputs.
{ lib }:
inputs:
let
  references =
    value:
    if builtins.isString value || builtins.isPath value || lib.isDerivation value then
      let
        text = builtins.toJSON value;
        context = builtins.getContext text;
        plain = builtins.unsafeDiscardStringContext text;
        contextPaths = lib.concatMap (
          path:
          let
            reference = context.${path};
            source = builtins.appendContext path { ${path}.path = true; };
            # Reading already-instantiated .drv metadata does not build outputs
            # or require import-from-derivation evaluation.
            metadata = builtins.import source;
            outputs =
              if reference.allOutputs or false then
                map (output: output.outPath) metadata.all
              else
                map (name: metadata.${name}.outPath) (reference.outputs or [ ]);
          in
          lib.optional ((reference.path or false) || (reference.allOutputs or false)) source ++ outputs
        ) (builtins.attrNames context);
        contains = path: lib.hasInfix (builtins.unsafeDiscardStringContext path) plain;
        tokens = map builtins.head (
          builtins.filter builtins.isList (
            builtins.split "(/nix/store/[0-9abcdfghijklmnpqrsvwxyz]{32}-[-a-zA-Z0-9+._?=]+)" plain
          )
        );
        # Contexts can name files whose contents reference other input-closure
        # paths. Preserve those references too; exportReferencesGraph still
        # requires every requested path to belong to the declared input closure.
        indirect = builtins.filter (
          token:
          !(lib.any (path: lib.hasPrefix (builtins.unsafeDiscardStringContext path) token) contextPaths)
        ) tokens;
      in
      if context == { } then
        [ ]
      else
        builtins.filter contains contextPaths ++ map (lib.strings.addContextFrom text) indirect
    else if builtins.isAttrs value then
      lib.concatMap references (builtins.attrValues value)
    else if builtins.isList value then
      lib.concatMap references value
    else
      [ ];
in
lib.sort builtins.lessThan (lib.unique (references inputs))
