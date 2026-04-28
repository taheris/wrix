{ inputs, ... }:

{
  perSystem =
    { pkgs, linuxPkgs, ... }:
    let
      shellcheck-batched = pkgs.writeShellApplication {
        name = "shellcheck-batched";
        runtimeInputs = [ pkgs.shellcheck ];
        text = builtins.readFile ../../lib/prek/shellcheck-batched.sh;
      };

      treefmtConfig = {
        projectRootFile = "flake.nix";
        programs = {
          deadnix.enable = true;
          nixfmt.enable = true;
          rustfmt.enable = true;
          statix.enable = true;
        };
        settings.formatter.shellcheck = {
          command = pkgs.lib.getExe shellcheck-batched;
          includes = [
            "*.sh"
            "*.bash"
          ];
          excludes = [ ".envrc" ];
          options = [ "--severity=warning" ];
        };
      };

    in
    {
      treefmt = treefmtConfig;

      _module.args.treefmtWrapper = inputs.treefmt-nix.lib.mkWrapper linuxPkgs treefmtConfig;
    };
}
