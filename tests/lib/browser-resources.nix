{
  root ? ../..,
}:
let
  inputDefinition = import ./inputs.nix { inherit root; };
  library = inputDefinition.directory (root + "/lib");
  fixtures = inputDefinition.directory (root + "/tests");
  contracts = inputDefinition.directory (root + "/specs");
  crates = inputDefinition.directory (root + "/crates");
in
{
  inherit library fixtures;
  inputs = inputDefinition.nix (
    library.inputs
    ++ fixtures.inputs
    ++ contracts.inputs
    ++ crates.inputs
    ++ [
      "/Cargo.toml"
      "/Cargo.lock"
      "/rust-toolchain.toml"
      "/docs/tmux.md"
      "/bin/test-ci-verifiers"
    ]
  );
}
