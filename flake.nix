{
  description = "Cross-platform sandbox for Claude Code";

  inputs = {
    nixpkgs.url = "git+ssh://git@github.com/NixOS/nixpkgs.git?ref=nixos-unstable&shallow=1";

    beads = {
      url = "git+ssh://git@github.com/gastownhall/beads.git?ref=main&shallow=1";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    fenix = {
      url = "git+https://github.com/nix-community/fenix.git?ref=main&shallow=1";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    flake-parts = {
      url = "git+ssh://git@github.com/hercules-ci/flake-parts.git?ref=main&shallow=1";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };

    gascity = {
      url = "git+ssh://git@github.com/gastownhall/gascity.git?ref=main&shallow=1";
      flake = false;
    };

    treefmt-nix = {
      url = "git+https://github.com/numtide/treefmt-nix.git?ref=main&shallow=1";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{ nixpkgs, flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } (
      { self, ... }:
      {
        imports = [
          inputs.treefmt-nix.flakeModule
        ];

        systems = [
          "aarch64-darwin"
          "aarch64-linux"
          "x86_64-linux"
        ];

        flake.nixosModules.city = ./modules/city.nix;
        flake.nixosModules.default = ./modules/city.nix;

        perSystem =
          {
            config,
            pkgs,
            self',
            system,
            ...
          }:
          let
            # Build beads-dolt and beads-push against a given pkgs scope.
            # Used by both hostOverlay and linuxOverlay so the wrapper scripts
            # are available as pkgs.beads-dolt / pkgs.beads-push everywhere.
            wrapixBeadsPkgs =
              hostPkgs_: linuxPkgs_:
              let
                m = import ./lib/beads {
                  pkgs = hostPkgs_;
                  linuxPkgs = linuxPkgs_;
                };
              in
              {
                beads-dolt = m.cli;
                beads-push = m.push;
              };

            hostOverlay =
              final: _prev:
              {
                beads = beadsFor final;
                gc = gcFor final;
              }
              // wrapixBeadsPkgs final linuxPkgs;

            beadsFor =
              pkgs':
              pkgs'.callPackage "${inputs.beads}/default.nix" {
                self = inputs.beads;
                buildGoModule =
                  args:
                  pkgs'.buildGoModule (
                    args
                    // {
                      proxyVendor = true;
                      vendorHash = "sha256-S/NavjGH6VSPU+rCtqtviOcGhgXc6VZUXCUhasSdUGU=";
                    }
                  );
              };

            gcFor =
              pkgs':
              pkgs'.buildGo126Module {
                pname = "gc";
                version = "dev";
                src = inputs.gascity;
                subPackages = [ "cmd/gc" ];
                proxyVendor = true;
                vendorHash = "sha256-59k7xFBaLZJ50KWNhwIzttE8j7GXZPneq6o4eUTlvBI=";
                doCheck = false;
              };

            linuxSystem = if system == "aarch64-darwin" then "aarch64-linux" else system;
            linuxOverlay =
              final: _prev:
              {
                beads = beadsFor final;
                gc = gcFor final;
              }
              // wrapixBeadsPkgs final final;
            linuxPkgs = import nixpkgs {
              system = linuxSystem;
              overlays = [ linuxOverlay ];
              config.allowUnfree = true;
            };

            sandboxPackages = [ (inputs.treefmt-nix.lib.mkWrapper linuxPkgs treefmt) ];

            test = import ./tests {
              inherit pkgs system linuxPkgs;
              src = self;
            };

            treefmt = {
              projectRootFile = "flake.nix";
              programs = {
                deadnix.enable = true;
                nixfmt.enable = true;
                rustfmt.enable = true;
                shellcheck = {
                  enable = true;
                  severity = "warning";
                };
                statix.enable = true;
              };
              settings.formatter.shellcheck.excludes = [ ".envrc" ];
            };

            wrapix = import ./lib {
              inherit pkgs system linuxPkgs;
              inherit (inputs) fenix;
            };
            city = wrapix.mkCity {
              name = "wx";
              profile = wrapix.profiles.base;
            };

          in
          {
            inherit treefmt;
            inherit (test) checks;

            _module.args.pkgs = import nixpkgs {
              inherit system;
              overlays = [
                hostOverlay
              ];
              config.allowUnfree = true;
            };

            legacyPackages.lib = {
              inherit (wrapix)
                deriveProfile
                mkCity
                mkDevShell
                mkRalph
                mkSandbox
                profiles
                ;
            };

            apps = {
              city = city.app;
              ralph = city.ralph.app;
              test = test.app;
              test-city = test.apps.city;
              test-ralph = test.apps.ralph;
              test-ralph-container = test.apps.ralph-container;
            };

            packages =
              let
                inherit (builtins) mapAttrs;
                inherit (wrapix) profiles;

                mkSandboxPkg =
                  cfg: (wrapix.mkSandbox (cfg // { packages = (cfg.packages or [ ]) ++ sandboxPackages; })).package;
                sandboxPkgs = mapAttrs (_: mkSandboxPkg) {
                  sandbox = {
                    profile = profiles.base;
                  };
                  sandbox-rust = {
                    profile = profiles.rust;
                  };
                  sandbox-python = {
                    profile = profiles.python;
                  };
                  sandbox-mcp = {
                    profile = profiles.base;
                    mcpRuntime = true;
                  };
                  sandbox-rust-mcp = {
                    profile = profiles.rust;
                    mcpRuntime = true;
                  };
                  sandbox-python-mcp = {
                    profile = profiles.python;
                    mcpRuntime = true;
                  };
                  debug = {
                    profile = profiles.base;
                    packages = [ linuxPkgs.podman ];
                  };
                };

              in
              sandboxPkgs
              // {
                inherit (pkgs) beads gc;
                default = sandboxPkgs.sandbox;
                city-config = city.configDir;
                tmux-mcp = import ./lib/mcp/tmux/mcp-server.nix { inherit pkgs; };
                wrapix-builder = import ./lib/builder { inherit pkgs linuxPkgs; };
                wrapix-notifyd = import ./lib/notify/daemon.nix { inherit pkgs; };
              };

            devShells.default = wrapix.mkDevShell {
              inherit (city) shellHook;
              packages = city.packages ++ [
                config.treefmt.build.wrapper
                pkgs.gh
                pkgs.podman
                self'.packages.wrapix-notifyd
              ];
            };
          };
      }
    );
}
