# Build-only dependency sources exclude unrelated workspace members and dev edges.
{
  pkgs,
  craneLib,
  root,
  packages,
  filter,
}:
let
  inherit (builtins)
    concatLists
    genericClosure
    readFile
    fromTOML
    attrNames
    attrValues
    isAttrs
    elem
    ;
  inherit (pkgs.lib)
    concatMap
    cleanSourceWith
    hasPrefix
    removePrefix
    optionalAttrs
    ;
  workspace = fromTOML (readFile (root + "/Cargo.toml"));
  lock = fromTOML (readFile (root + "/Cargo.lock"));
  directories = builtins.filter (name: builtins.pathExists (root + "/crates/${name}/Cargo.toml")) (
    attrNames (builtins.readDir (root + "/crates"))
  );
  members = builtins.listToAttrs (
    map (directory: {
      name = (fromTOML (readFile (root + "/crates/${directory}/Cargo.toml"))).package.name;
      value = directory;
    }) directories
  );
  manifests = builtins.mapAttrs (
    _: directory: fromTOML (readFile (root + "/crates/${directory}/Cargo.toml"))
  ) members;
  tables = value: [
    (value.dependencies or { })
    (value.build-dependencies or { })
  ];
  dependencies =
    name:
    let
      manifest = manifests.${name};
      values = concatLists (map tables ([ manifest ] ++ attrValues (manifest.target or { })));
      resolve =
        alias: dependency:
        let
          inherited = isAttrs dependency && (dependency.workspace or false);
          specification = if inherited then workspace.workspace.dependencies.${alias} else dependency;
          base = if inherited then root else root + "/crates/${members.${name}}";
          path =
            if isAttrs specification && specification ? path then base + "/${specification.path}" else null;
          matches = builtins.filter (
            candidate: toString (root + "/crates/${members.${candidate}}") == toString path
          ) (attrNames members);
        in
        if path == null then
          [ ]
        else
          assert pkgs.lib.assertMsg (
            builtins.length matches == 1
          ) "Production dependency ${name}.${alias} must resolve to one actual workspace member";
          matches;
    in
    concatMap (table: concatMap (alias: resolve alias table.${alias}) (attrNames table)) values;
  selected = map (item: item.key) (genericClosure {
    startSet = map (key: { inherit key; }) packages;
    operator = item: map (key: { inherit key; }) (dependencies item.key);
  });
  selectedDirectories = map (name: members.${name}) selected;
  source = cleanSourceWith {
    src = root;
    name = "wrix-production-dependency-inputs";
    filter =
      path: type:
      let
        relative = removePrefix "${toString root}/" path;
        unused = builtins.any (
          directory:
          !(elem directory selectedDirectories)
          && (relative == "crates/${directory}" || hasPrefix "crates/${directory}/" relative)
        ) directories;
      in
      !unused && filter path type;
  };
  normalManifest =
    manifest:
    builtins.removeAttrs manifest [ "dev-dependencies" ]
    // optionalAttrs (manifest ? target) {
      target = builtins.mapAttrs (
        _: value: builtins.removeAttrs value [ "dev-dependencies" ]
      ) manifest.target;
    };
  scopedCrane = craneLib.overrideScope (
    _: previous: {
      cleanCargoToml =
        arguments:
        let
          manifest = normalManifest (previous.cleanCargoToml arguments);
        in
        manifest
        // optionalAttrs (manifest ? workspace) {
          workspace = builtins.removeAttrs manifest.workspace [ "default-members" ] // {
            members = map (name: "crates/${members.${name}}") selected;
          };
        };
    }
  );
  pins = pkgs.writeText "canonical-dependency-pins.json" (
    builtins.toJSON (
      map (package: {
        inherit (package) name version;
        source = package.source or null;
      }) lock.package
    )
  );
in
{
  inherit selected;
  src = scopedCrane.mkDummySrc {
    src = source;
    cargoToml = builtins.path {
      path = root + "/Cargo.toml";
      name = "Cargo.toml";
    };
    cargoLock = builtins.path {
      path = root + "/Cargo.lock";
      name = "Cargo.lock";
    };
  };
  preBuild = ''
    cargo metadata --offline --format-version 1 > "$TMPDIR/production-dependency-metadata.json" || exit 1
    ${pkgs.jq}/bin/jq -e --slurpfile canonical ${pins} '
      all(.packages[]; . as $package |
        any($canonical[0][]; .name == $package.name and .version == $package.version and .source == $package.source))
    ' "$TMPDIR/production-dependency-metadata.json" > /dev/null || {
      echo 'Production dependency resolution introduced a package outside canonical Cargo.lock' >&2
      exit 1
    }
    cargo metadata --offline --locked --format-version 1 > /dev/null
  '';
}
