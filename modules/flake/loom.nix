{ inputs, ... }:

{
  perSystem =
    { pkgs, linuxPkgs, ... }:
    let
      loomLib = import "${inputs.loom-src}/nix/lib.nix";
      mkLoom =
        pkgs:
        (loomLib.mkLoom {
          inherit pkgs;
          inherit (inputs) crane fenix;
          src = inputs.loom-src;
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
        inherit loomCli linuxLoomCli;
      };
      packages.loom = loomCli;
    };
}
