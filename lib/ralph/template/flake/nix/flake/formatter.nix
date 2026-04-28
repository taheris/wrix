_: {
  perSystem = _: {
    treefmt = {
      projectRootFile = "flake.nix";
      programs = {
        deadnix.enable = true;
        nixfmt.enable = true;
        shellcheck.enable = true;
        statix.enable = true;
      };
      settings.formatter = {
        shellcheck.excludes = [ ".envrc" ];
      };
    };
  };
}
