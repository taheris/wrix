# Executed by a separate read-only evaluator with a dummy store, not imported
# into the API check's build graph. The output must never be realised.
{
  lib,
  candidates,
  system,
}:
let
  output = builtins.derivation {
    name = "configuration-readonly-native-regression";
    inherit system;
    builder = "/unavailable-configuration-readonly-builder";
  };
  reference = "${output}";
  metadata = builtins.attrNames (builtins.getContext reference);
in
assert lib.all (path: !builtins.pathExists path) metadata;
assert
  candidates {
    nested = [
      reference
      "${reference}/bin/tool"
    ];
  } == [ reference ];
true
