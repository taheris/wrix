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
  scopedNix =
    inputs:
    definitions.scopedNix inputs
    ++ [
      "/flake.nix"
      "/modules/flake/"
      "/tests/default.nix"
      "/tests/verify/"
    ];
  imageInputs = scopedNix (
    sandbox.inputs
    ++ [
      "/Cargo.toml"
      "/Cargo.lock"
      "/clippy.toml"
      "/crates/**/Cargo.toml"
      "/crates/**/lib.rs"
      "/crates/**/main.rs"
      "/crates/wrix-image-tools/"
      "/crates/wrix-verify-tools/"
      "/lib/verify/"
    ]
    ++ mcp.inputs
    ++ sandboxFixtures.inputs
    ++ directories [
      "lib/notify"
      "lib/prek"
      "lib/profile"
      "lib/beads"
      "crates/wrix-service/src/lifecycle/drain/native"
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
      "/lib/default.nix"
      "/bin/pre-push-checks"
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
  browserInputs = scopedNix (
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
