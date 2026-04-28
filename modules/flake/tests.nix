{ self, ... }:

{
  perSystem =
    {
      pkgs,
      system,
      linuxPkgs,
      treefmtWrapper,
      ...
    }:
    let
      test = import ../../tests {
        inherit pkgs system linuxPkgs;
        treefmt = treefmtWrapper;
        src = self;
      };

    in
    {
      _module.args.test = test;

      inherit (test) checks;
    };
}
