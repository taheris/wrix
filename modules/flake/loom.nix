{ inputs, ... }:

{
  perSystem =
    { pkgs, linuxPkgs, ... }:
    let
      upstreamLib = import "${inputs.loom-src}/nix/lib.nix";
      loomLib = upstreamLib // {
        mkProfileManifest = import ../../lib/loom-manifest.nix { upstream = upstreamLib; };
      };
      launchPatches = [
        ../../patches/loom-data-only-manifests.patch
        ../../patches/loom-spawn-extensions.patch
      ];
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
          patches = (old.patches or [ ]) ++ launchPatches;
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
          patches = launchPatches;
          nativeBuildInputs = [ pkgs.git ];
          cargoNextestExtraArgs = "-p loom-driver -E 'test(wrix_launch_) | test(wrix_runner_) | test(profile_manifest) | test(resolve_launcher)'";
        };
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
