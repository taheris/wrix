# Preserve Cargo's conventional target membership without unselected source bytes.
{
  pkgs,
  root,
  members,
  includeProduction ? true,
  includeDevelopment ? true,
  minimalProduction ? false,
}:
let
  inherit (pkgs.lib) concatMapStringsSep escapeShellArg;
  conventional =
    directory: prefix:
    let
      entries = if builtins.pathExists directory then builtins.readDir directory else { };
    in
    map (entry: "${prefix}/${entry}") (
      builtins.filter (entry: builtins.pathExists (directory + "/${entry}")) (
        builtins.concatLists (
          map (
            entry:
            if entries.${entry} == "directory" then
              [ "${entry}/main.rs" ]
            else if pkgs.lib.hasSuffix ".rs" entry then
              [ entry ]
            else
              [ ]
          ) (builtins.attrNames entries)
        )
      )
    );
  entrypoints =
    name:
    let
      directory = root + "/crates/${name}";
      production = builtins.filter (entry: builtins.pathExists (directory + "/${entry}")) [
        "src/lib.rs"
        "src/main.rs"
      ];
    in
    (
      if !includeProduction then
        [ ]
      else if minimalProduction then
        if production != [ ] then
          production
        else
          pkgs.lib.take 1 (conventional (directory + "/src/bin") "src/bin")
      else
        production ++ conventional (directory + "/src/bin") "src/bin"
    )
    ++ (
      if includeDevelopment then
        builtins.concatLists (
          map (kind: conventional (directory + "/${kind}") kind) [
            "tests"
            "examples"
            "benches"
          ]
        )
      else
        [ ]
    );
in
pkgs.runCommand "cargo-target-metadata" { } ''
  set -euo pipefail
  mkdir -p "$out"
  ${concatMapStringsSep "\n" (
    name:
    concatMapStringsSep "\n" (entry: ''
      mkdir -p "$out"/${escapeShellArg (builtins.dirOf "crates/${name}/${entry}")}
      touch "$out"/${escapeShellArg "crates/${name}/${entry}"}
    '') (entrypoints name)
  ) members}
''
