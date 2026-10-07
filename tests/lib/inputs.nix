# Resource operands supply both execution and its input projection. Opaque
# definitions do not use this constructor: their declarations remain unknown.
{
  root ? ../..,
}:

let
  inherit (builtins)
    all
    attrNames
    filter
    isList
    isString
    listToAttrs
    readFile
    stringLength
    substring
    toString
    ;
  rootPrefix = "${toString root}/";
  relative =
    path:
    let
      absolute = toString path;
      prefixLength = stringLength rootPrefix;
    in
    assert substring 0 prefixLength absolute == rootPrefix;
    substring prefixLength (stringLength absolute - prefixLength) absolute;
in
{
  project =
    definitions:
    listToAttrs (
      filter (entry: entry.value != null) (
        map (name: {
          inherit name;
          value =
            let
              inputs = definitions.${name}.inputs or null;
            in
            if inputs == null then
              null
            else
              assert isList inputs && all isString inputs;
              inputs;
        }) (attrNames definitions)
      )
    );
  read = path: {
    inputs = [ "/${path}" ];
    text = readFile (rootPrefix + path);
  };
  directory = path: {
    inherit path;
    inputs = [ "/${relative path}/" ];
  };
  # Nix evaluation/packaging can depend on any repository Nix definition.
  # Include new definitions, locked tools and the actual shared dispatch code;
  # this is deliberately broader than the resource operands, not a per-ID map.
  nix =
    inputs:
    [
      "/**/*.nix"
      "/flake.lock"
      "/loom.toml"
      "/${relative ./verifier.sh}"
      "/${relative ./print-inputs.sh}"
    ]
    ++ inputs;
}
