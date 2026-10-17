{ lib }:
let
  inherit (lib) evalModules mkOption types;
  capabilities.imagePolicies = {
    default = [ 1 ];
    private-native-nix = [ 1 ];
  };
  resolve =
    policy:
    let
      evaluated = evalModules {
        modules = [
          {
            options = {
              kind = mkOption { type = types.enum (builtins.attrNames capabilities.imagePolicies); };
              version = mkOption { type = types.addCheck types.int (version: version == 1); };
            };
          }
          { config = policy; }
        ];
      };
      normalized = { inherit (evaluated.config) kind version; };
    in
    if !builtins.isAttrs policy then
      throw "mkSandbox.imagePolicy must be a typed policy attribute set"
    else
      builtins.deepSeq normalized normalized;
in
{
  inherit capabilities resolve;
}
