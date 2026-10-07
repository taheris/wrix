{ pkgs }:

let
  inherit (builtins)
    deepSeq
    elem
    readFile
    tryEval
    ;
  definitions = import ./inputs.nix { };
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
in
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
