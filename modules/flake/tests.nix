{ self, inputs, ... }:

{
  perSystem =
    {
      pkgs,
      system,
      linuxPkgs,
      treefmtWrapper,
      wrapix,
      ...
    }:
    let
      test = import ../../tests {
        inherit
          pkgs
          system
          linuxPkgs
          wrapix
          ;
        treefmt = treefmtWrapper;
        src = self;
        inherit (inputs) fenix;
      };

    in
    {
      _module.args.test = test;

      inherit (test) checks;

      # `loom-tests` is the spec-aligned design target for
      # `loom gate verify` (see specs/loom-tests.md Nix Integration). It
      # is exposed under `packages` rather than `checks` until every
      # [check]/[test] annotation across `specs/*.md` resolves against
      # the verifier-runner contract; `nix build .#loom-tests` invokes
      # it incrementally during that migration.
      packages.loom-tests = test.loomChecks.loom-tests;
    };
}
