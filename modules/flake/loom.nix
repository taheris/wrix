{ inputs, ... }:

{
  perSystem =
    { pkgs, linuxPkgs, ... }:
    let
      loomSource = pkgs.applyPatches {
        name = "loom-source";
        src = inputs.loom-src;
        patches = [
          ../../patches/loom-system-input-batching.patch
          ../../patches/loom-data-only-manifests.patch
        ];
      };
      loomLib = import "${loomSource}/nix/lib.nix";
      mkLoom =
        pkgs:
        loomLib.mkLoom {
          inherit pkgs;
          inherit (inputs) crane fenix;
          src = loomSource;
        }).bin.overrideAttrs
          (old: {
            postInstall = (old.postInstall or "") + ''
              find "$out/bin" -type f ! -name loom ! -name loom-walk -delete
            '';
          });
      loomCli = mkLoom pkgs;
      linuxLoomCli = mkLoom linuxPkgs;
    in
    {
      _module.args = {
        inherit loomCli linuxLoomCli loomLib;
      };
      packages.loom = loomCli;
      checks.loom-spawn-contract = spawnContract;
    };
}
