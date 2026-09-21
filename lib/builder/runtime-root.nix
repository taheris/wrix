{ pkgs, seedRoots }:

if seedRoots == [ ] then
  throw "wrix-builder requires runtime roots for its persistent Nix store"
else
  pkgs.writeText "wrix-builder-runtime-roots" (
    pkgs.lib.concatMapStrings (root: "${root}\n") seedRoots
  )
