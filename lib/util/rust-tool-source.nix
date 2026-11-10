# Keep unused workspace members discoverable without consuming their implementation.
{
  pkgs,
  root,
  package,
  extraSources ? [ ],
}:
let
  workspace = builtins.fromTOML (builtins.readFile (root + "/Cargo.toml"));
  manifest = builtins.fromTOML (builtins.readFile (root + "/crates/${package}/Cargo.toml"));
  dependencyTables = value: [
    (value.dependencies or { })
    (value.dev-dependencies or { })
    (value.build-dependencies or { })
  ];
  dependencies = builtins.concatLists (
    map dependencyTables ([ manifest ] ++ builtins.attrValues (manifest.target or { }))
  );
  localDependency =
    name: dependency:
    dependency ? path
    || (dependency.workspace or false && (workspace.workspace.dependencies.${name} or { }) ? path);
  standalone = builtins.all (
    table: builtins.all (name: !(localDependency name table.${name})) (builtins.attrNames table)
  ) dependencies;
  members = builtins.filter (name: name != package) (
    builtins.attrNames (builtins.readDir (root + "/crates"))
  );
  metadata = import ./rust-target-metadata.nix { inherit pkgs root members; };
  source = pkgs.lib.fileset.toSource {
    inherit root;
    fileset = pkgs.lib.fileset.unions (
      [
        (root + "/Cargo.toml")
        (root + "/Cargo.lock")
        (root + "/clippy.toml")
        (pkgs.lib.fileset.fileFilter (file: file.name == "Cargo.toml") (root + "/crates"))
        (root + "/crates/${package}")
      ]
      ++ extraSources
    );
  };
in
assert pkgs.lib.assertMsg standalone
  "rust-tool requires complete sources for local Cargo dependencies";
pkgs.runCommand "${package}-source" { } ''
  set -euo pipefail
  cp -r ${source}/. "$out"
  chmod -R u+w "$out"
  cp -r ${metadata}/. "$out"
  chmod -R u+w "$out"
''
