{
  pkgs,
  self,
  system,
}:
let
  descriptions = import ../verify/profiles-eval.nix {
    root = self;
    inherit system;
    target = "";
    describe = true;
  };
  targets = builtins.attrNames descriptions;
  direct =
    target:
    import ../verify/profiles-eval.nix {
      root = self;
      flake = self;
      inherit system target;
    };
  cached = self.legacyPackages.${system}.verifyEvaluations;
  broken = self // {
    legacyPackages = self.legacyPackages // {
      ${system} = self.legacyPackages.${system} // {
        lib = self.legacyPackages.${system}.lib // {
          devToolchain = "forbidden";
        };
      };
    };
  };
  rejected = builtins.tryEval (
    import ../verify/profiles-eval.nix {
      root = self;
      flake = broken;
      inherit system;
      target = "profiles.no-dev-toolchain-lib";
    }
  );
in
assert builtins.all (
  target: cached.${target} == direct target && cached.${target} == "passed"
) targets;
assert !rejected.success;
pkgs.runCommand "verify-evaluation-cache-conformance" { } ''touch "$out"''
