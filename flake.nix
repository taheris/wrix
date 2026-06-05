{
  description = "Cross-platform sandbox for Claude Code";

  inputs = {
    nixpkgs.url = "git+https://github.com/NixOS/nixpkgs.git?ref=nixos-unstable&shallow=1";

    crane = {
      url = "git+https://github.com/ipetkov/crane.git?ref=master&shallow=1";
    };

    fenix = {
      url = "git+https://github.com/nix-community/fenix.git?ref=main&shallow=1";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    flake-parts = {
      url = "git+https://github.com/hercules-ci/flake-parts.git?ref=main&shallow=1";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };

    treefmt-nix = {
      url = "git+https://github.com/numtide/treefmt-nix.git?ref=main&shallow=1";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        inputs.treefmt-nix.flakeModule
        ./modules/flake/apps.nix
        ./modules/flake/devshell.nix
        ./modules/flake/formatter.nix
        ./modules/flake/lib.nix
        ./modules/flake/overlays.nix
        ./modules/flake/packages.nix
        ./modules/flake/tests.nix
      ];

      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];
    };
}
