{ pkgs }:

let
  inherit (builtins)
    deepSeq
    elem
    readFile
    tryEval
    ;
  definitions = import ./inputs.nix { };
  imageResources = import ./image-resources.nix { };
  builders = import ../builder/checks.nix {
    pkgs = throw "input discovery evaluated builder tools";
    linuxPkgs = throw "input discovery evaluated an image";
  };
  profiles = import ../verify/profiles-eval.nix {
    root = ../..;
    system = throw "input discovery evaluated profile tools";
    target = "";
    describe = true;
  };
  declared = definitions.project {
    known = {
      inputs = [ "/resource" ];
      passed = throw "input discovery executed a predicate";
      package = throw "input discovery evaluated a package";
    };
    opaque = {
      package = throw "opaque package was evaluated";
    };
    unknown = {
      inputs = null;
      passed = throw "opaque predicate was executed";
    };
  };
  malformed =
    inputs:
    (tryEval (
      deepSeq (definitions.project {
        fixture = { inherit inputs; };
      }) true
    )).success;
  first = definitions.read "tests/lib/inputs.nix";
  second = definitions.read "tests/lib/print-inputs.sh";
  directory = definitions.directory ../builder;
  lazyVerifier = import ../verify {
    inherit pkgs;
    system = pkgs.stdenv.hostPlatform.system;
    linuxPkgs = throw "inventory evaluated image packages";
    focusBinary = throw "inventory evaluated an opaque runtime";
    src = ../..;
  };
  loom = import ../loom.nix {
    inherit pkgs;
    linuxPkgs = throw "discovery evaluated Loom image packages";
    wrix = throw "discovery evaluated Loom sandbox construction";
    loomCli = throw "discovery evaluated Loom tooling";
    linuxLoomCli = throw "discovery evaluated Linux Loom tooling";
    packages = throw "discovery evaluated profile packages";
    devShell = throw "discovery evaluated the devshell";
    lock = throw "discovery evaluated lock assertions";
  };
  loomInputs = definitions.project (
    builtins.listToAttrs (
      map (app: {
        inherit (app) name;
        value = app;
      }) loom.ciApps
    )
  );
  lazyCi = import ../default.nix {
    inherit pkgs;
    loomTests = loom;
    system = pkgs.stdenv.hostPlatform.system;
    linuxPkgs = throw "CI inventory evaluated image packages";
    wrix = throw "CI inventory evaluated a runtime predicate";
    crane = throw "CI inventory evaluated compiler tools";
    fenix = throw "CI inventory evaluated toolchains";
    treefmt = throw "CI inventory evaluated formatting tools";
    src = ../..;
  };
  darwinBrowser = import ../verify/playwright-mcp.nix {
    pkgs = {
      inherit (pkgs) lib;
      stdenv.hostPlatform.isDarwin = true;
    };
    system = "aarch64-darwin";
  };
  browserChecks = import ../mcp/playwright/check.nix {
    pkgs = throw "description evaluated browser tools";
    linuxPkgs = throw "description evaluated browser packages";
    system = throw "description evaluated the runtime platform";
    crane = throw "description evaluated compiler tools";
    fenix = throw "description evaluated toolchains";
    treefmt = throw "description evaluated formatting tools";
  };
in
assert builtins.length (builtins.attrNames loomInputs) == builtins.length loom.ciApps;
assert deepSeq lazyVerifier.package.drvPath true;
assert deepSeq lazyCi.testCi.drvPath true;
assert deepSeq browserChecks.chromiumImageCheck.inputs true;
assert elem "/specs/playwright-mcp.md" browserChecks.executablePathCheck.inputs;
assert !(elem "/crates/" browserChecks.executablePathCheck.inputs);
assert !(elem "/specs/" browserChecks.executablePathCheck.inputs);
assert elem "/crates/" imageResources.runtimeInputs;
assert elem "/crates/" darwinBrowser."playwright-mcp.screenshot".inputs;
assert elem "/lib/sandbox/" darwinBrowser."playwright-mcp.smoke".inputs;
assert !(elem "/crates/" darwinBrowser."playwright-mcp.registry-triple".inputs);
assert elem "/lib/services/" imageResources.runtimeInputs;
assert !(elem "/crates/" imageResources.imageInputs);
assert !(elem "/specs/" imageResources.imageInputs);
assert elem "/tests/sandbox/" imageResources.imageInputs;
assert elem "/specs/image-builder.md" imageResources.imageInputs;
assert elem "/lib/sandbox/" browserChecks.chromiumImageCheck.inputs;
assert declared == { known = [ "/resource" ]; };
assert !malformed "not-an-array";
assert !malformed [ null ];
assert first.inputs == [ "/tests/lib/inputs.nix" ] && first.text == readFile ./inputs.nix;
assert
  second.inputs == [ "/tests/lib/print-inputs.sh" ] && second.text == readFile ./print-inputs.sh;
assert directory.inputs == [ "/tests/builder/" ] && directory.path == ../builder;
assert builders.sshdHardeningTest.inputs == builders.imageSourceKindTest.inputs;
assert builders.sourceKindLoadTransportTest.inputs == builders.imageSourceKindTest.inputs;
assert elem "/lib/" builders.sshdHardeningTest.inputs;
assert profiles."devshell.sandbox-boundary" == null;
assert elem "/lib/devshell/default.nix" profiles."devshell.no-prek-install";
pkgs.runCommandLocal "verifier-inputs" { } ''
  touch "$out"
''
