{
  description = "wrapix project";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    treefmt-nix.url = "github:numtide/treefmt-nix";
    wrapix.url = "github:taheris/wrapix";
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      imports = [ inputs.treefmt-nix.flakeModule ];

      perSystem =
        {
          config,
          pkgs,
          system,
          ...
        }:
        let
          inherit (inputs.wrapix.packages.${system}) ralph;
          wrapixLib = inputs.wrapix.legacyPackages.${system}.lib;
          sandbox = wrapixLib.mkSandbox { profile = wrapixLib.profiles.base; };
        in
        {
          apps.sandbox = {
            type = "app";
            program = "${sandbox.package}/bin/wrapix";
          };

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
    };
}
