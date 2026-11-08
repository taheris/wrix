{ pkgs, ciApps }:

let
  inherit (pkgs.lib) concatMap hasSuffix;
  registered = map (app: app.name) ciApps;
  extract =
    text:
    map builtins.head (
      builtins.filter builtins.isList (builtins.split "[[]check[]][(]test-ci:([^)]*)[)]" text)
    );
  unknown = names: builtins.filter (name: !(builtins.elem name registered)) names;
  specs = builtins.filter (hasSuffix ".md") (builtins.attrNames (builtins.readDir ../../specs));
  invalid = concatMap (
    file:
    map (name: "${file}: ${name}") (unknown (extract (builtins.readFile (../../specs + "/${file}"))))
  ) specs;
in
assert extract "[check](verify:other) [check](test-ci:example)" == [ "example" ];
assert unknown [ "__registry_negative_control__" ] == [ "__registry_negative_control__" ];
assert unknown registered == [ ];
assert pkgs.lib.assertMsg (
  invalid == [ ]
) "Unregistered test-ci annotations: ${builtins.concatStringsSep ", " invalid}";
pkgs.runCommand "ci-annotation-registration" { } ''
  touch "$out"
''
