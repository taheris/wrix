{ inputs, ... }:

{
  perSystem =
    { pkgs, linuxPkgs, ... }:
    let
      loomLib = import "${inputs.loom-src}/nix/lib.nix";
      spawnPatches = [ ../../patches/loom-spawn-extensions.patch ];
      loomWorkspace =
        pkgs:
        loomLib.mkLoom {
          inherit pkgs;
          inherit (inputs) crane fenix;
          src = inputs.loom-src;
        };
      mkLoom =
        pkgs:
        (loomWorkspace pkgs).bin.overrideAttrs (old: {
          patches = (old.patches or [ ]) ++ spawnPatches;
          postInstall = (old.postInstall or "") + ''
            find "$out/bin" -type f ! -name loom ! -name loom-walk -delete
          '';
        });
      spawnContract =
        let
          workspace = loomWorkspace pkgs;
        in
        workspace.craneLib.cargoNextest {
          src = workspace.stagedSrc;
          inherit (workspace) cargoArtifacts;
          patches = spawnPatches;
          nativeBuildInputs = [ pkgs.git ];
          cargoNextestExtraArgs = "-p loom-driver -E 'test(wrix_launch_) | test(wrix_runner_)'";
        };
      loomCli = mkLoom pkgs;
      linuxLoomCli = mkLoom linuxPkgs;
    in
    {
      _module.args = {
        inherit loomCli linuxLoomCli;
      };
      packages.loom = loomCli;
      checks.loom-spawn-contract = spawnContract;
    };
}
