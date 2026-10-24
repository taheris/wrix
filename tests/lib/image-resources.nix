{
  root ? ../..,
}:
let
  definitions = import ./inputs.nix { inherit root; };
  sandbox = definitions.directory (root + "/lib/sandbox");
  mcp = definitions.directory (root + "/lib/mcp");
  sandboxFixtures = definitions.directory (root + "/tests/sandbox");
  browserFixtures = definitions.directory (root + "/tests/mcp");
  directories =
    paths: builtins.concatLists (map (path: (definitions.directory (root + "/${path}")).inputs) paths);
  contracts = names: map (name: "/specs/${name}.md") names;
  imageInputs = definitions.nix (
    sandbox.inputs
    ++ mcp.inputs
    ++ sandboxFixtures.inputs
    ++ directories [
      "lib/notify"
      "lib/prek"
      "lib/util"
      "tests/lib"
      "tests/fixtures"
    ]
    ++ contracts [
      "image-builder"
      "sandbox"
      "profiles"
      "security"
      "pre-commit"
      "store"
      "playwright-mcp"
    ]
    ++ [
      "/docs/tmux.md"
      "/bin/test-ci-verifiers"
    ]
  );
  runtimeInputs =
    imageInputs
    ++ directories [
      "crates"
      "lib/services"
      "lib/builder"
    ]
    ++ [
      "/Cargo.toml"
      "/Cargo.lock"
    ];
  browserInputs = definitions.nix (
    mcp.inputs
    ++ browserFixtures.inputs
    ++ contracts [ "playwright-mcp" ]
    ++ [
      "/bin/test-ci-verifiers"
      "/tests/sandbox/pi-mcp-native.mjs"
    ]
  );
in
{
  inherit
    sandbox
    mcp
    sandboxFixtures
    browserFixtures
    imageInputs
    runtimeInputs
    browserInputs
    ;
  inputs = imageInputs;
  browserImageInputs = imageInputs ++ browserInputs;
}
