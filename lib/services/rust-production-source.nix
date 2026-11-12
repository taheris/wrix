# Production implementations follow normal/build local dependencies, while every
# canonical manifest and Cargo target remains discoverable in the workspace.
{
  pkgs,
  root,
  filter,
  packages,
}:
let
  inherit (pkgs) lib;
  inherit (lib) concatMap hasPrefix removePrefix;
  workspace = builtins.fromTOML (builtins.readFile (root + "/Cargo.toml"));
  directories = builtins.filter (name: builtins.pathExists (root + "/crates/${name}/Cargo.toml")) (
    builtins.attrNames (builtins.readDir (root + "/crates"))
  );
  members = builtins.listToAttrs (
    map (directory: {
      name =
        (builtins.fromTOML (builtins.readFile (root + "/crates/${directory}/Cargo.toml"))).package.name;
      value = directory;
    }) directories
  );
  manifests = builtins.mapAttrs (
    _: directory: builtins.fromTOML (builtins.readFile (root + "/crates/${directory}/Cargo.toml"))
  ) members;
  tables = value: [
    (value.dependencies or { })
    (value.build-dependencies or { })
  ];
  dependencies =
    name:
    let
      manifest = manifests.${name};
      values = builtins.concatLists (
        map tables ([ manifest ] ++ builtins.attrValues (manifest.target or { }))
      );
      resolve =
        alias: dependency:
        let
          inherited = builtins.isAttrs dependency && (dependency.workspace or false);
          specification = if inherited then workspace.workspace.dependencies.${alias} else dependency;
          base = if inherited then root else root + "/crates/${members.${name}}";
          path =
            if builtins.isAttrs specification && specification ? path then
              base + "/${specification.path}"
            else
              null;
          matches = builtins.filter (
            candidate: toString (root + "/crates/${members.${candidate}}") == toString path
          ) (builtins.attrNames members);
        in
        if path == null then
          [ ]
        else
          assert lib.assertMsg (
            builtins.length matches == 1
          ) "Client local dependency ${name}.${alias} is outside the declared workspace source";
          matches;
    in
    concatMap (
      table: concatMap (alias: resolve alias table.${alias}) (builtins.attrNames table)
    ) values;
  selected = map (item: item.key) (
    builtins.genericClosure {
      startSet = map (key: { inherit key; }) packages;
      operator = item: map (key: { inherit key; }) (dependencies item.key);
    }
  );
  selectedDirectories = map (name: members.${name}) selected;
  unusedDirectories = builtins.filter (
    directory: !(builtins.elem directory selectedDirectories)
  ) directories;
  selectedSource = lib.cleanSourceWith {
    src = root;
    name = "wrix-production";
    filter =
      path: type:
      let
        relative = removePrefix "${toString root}/" path;
        unused = builtins.any (
          directory: hasPrefix "crates/${directory}/" relative && relative != "crates/${directory}/Cargo.toml"
        ) unusedDirectories;
      in
      !unused && filter path type;
  };
  unusedTargets = import ../util/rust-target-metadata.nix {
    inherit pkgs root;
    members = unusedDirectories;
  };
  selectedTargets = import ../util/rust-target-metadata.nix {
    inherit pkgs root;
    members = selectedDirectories;
    includeProduction = false;
  };
in
pkgs.runCommand "wrix-production-source"
  {
    passthru = { inherit selected; };
  }
  ''
    set -euo pipefail
    mkdir -p "$out"
    cp -r ${selectedSource}/. "$out"
    chmod -R u+w "$out"
    cp -r ${unusedTargets}/. "$out"
    cp -r ${selectedTargets}/. "$out"
  ''
