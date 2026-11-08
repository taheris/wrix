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
  # Spec acceptance criteria are top-level dash items. Check CI items here so
  # mixed-runner annotations fail before the more expensive canonical gate.
  atomic =
    text:
    builtins.all (
      block:
      let
        item = builtins.head (builtins.split "\n\n" block);
      in
      extract item == [ ]
      ||
        builtins.length (
          builtins.filter builtins.isList (builtins.split "[[](check|test|system|judge)[]][(]([^)]*)[)]" item)
        ) == 1
    ) (builtins.filter builtins.isString (builtins.split "\n- " ("\n" + text)));
  specs = builtins.filter (hasSuffix ".md") (builtins.attrNames (builtins.readDir ../../specs));
  invalid = concatMap (
    file:
    let
      text = builtins.readFile (../../specs + "/${file}");
    in
    map (name: "${file}: ${name}") (unknown (extract text))
    ++ pkgs.lib.optional (!(atomic text)) "${file}: CI criterion has multiple annotations"
  ) specs;
in
assert extract "[check](verify:other) [check](test-ci:example)" == [ "example" ];
assert unknown [ "__registry_negative_control__" ] == [ "__registry_negative_control__" ];
assert unknown registered == [ ];
assert atomic "- One [check](test-ci:example)\n- Two [check](verify:other)";
assert !(atomic "- One [check](test-ci:example) [check](verify:other)");
assert atomic "- Discussion\n\nProse [check](test-ci:example) [check](verify:other)";
assert pkgs.lib.assertMsg (
  invalid == [ ]
) "Unregistered test-ci annotations: ${builtins.concatStringsSep ", " invalid}";
pkgs.runCommand "ci-annotation-registration" { } ''
  touch "$out"
''
