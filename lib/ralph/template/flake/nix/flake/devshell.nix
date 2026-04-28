{ inputs, ... }:
{
  perSystem =
    {
      config,
      pkgs,
      system,
      ...
    }:
    let
      inherit (inputs.wrapix.packages.${system}) ralph;
    in
    {
      devShells.default = pkgs.mkShell {
        packages = [
          ralph
          inputs.wrapix.packages.${system}.beads
          config.treefmt.build.wrapper
        ];
        shellHook = ''
          ${ralph.shellHook}
        '';
      };
    };
}
