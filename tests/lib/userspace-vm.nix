# Test guests use upstream 9p without requiring host virtiofsd capabilities.
{ pkgs }:
{ lib, ... }:

let
  nixpkgs = toString pkgs.path;
  source = builtins.readFile (pkgs.path + "/nixos/modules/virtualisation/qemu-vm.nix");
  selection = "useVirtiofs = hostPkgs.stdenv.hostPlatform.isLinux;";
  replacements = {
    "${selection}" = "useVirtiofs = false;";
    "../../lib/" = "${nixpkgs}/nixos/lib/";
    "../profiles/" = "${nixpkgs}/nixos/modules/profiles/";
    "./disk-size-option.nix" = "${nixpkgs}/nixos/modules/virtualisation/disk-size-option.nix";
    "./credentials-options.nix" = "${nixpkgs}/nixos/modules/virtualisation/credentials-options.nix";
  };
  module = builtins.toFile "userspace-qemu-vm.nix" (
    builtins.replaceStrings (builtins.attrNames replacements) (builtins.attrValues replacements) source
  );
in
assert lib.assertMsg (lib.hasInfix selection source)
  "pinned QEMU module transport selection changed";
{
  disabledModules = [ "virtualisation/qemu-vm.nix" ];
  imports = [ module ];
}
