{ inputs, ... }:

{
  perSystem =
    { pkgs, linuxPkgs, ... }:
    let
      upstreamLib = import "${inputs.loom-src}/nix/lib.nix";
      loomLib = upstreamLib // {
        mkProfileManifest = import ../../lib/loom-manifest.nix { upstream = upstreamLib; };
      };
      loomPatches = [
        ../../patches/loom-data-only-manifests.patch
        ../../patches/loom-spawn-extensions.patch
        ../../patches/loom-runner-input-batching.patch
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
          patches = (old.patches or [ ]) ++ loomPatches;
          postInstall = (old.postInstall or "") + ''
            find "$out/bin" -type f ! -name loom ! -name loom-walk -delete
          '';
        });
      loomContract =
        cargoNextestExtraArgs:
        let
          workspace = loomWorkspace pkgs;
        in
        workspace.craneLib.cargoNextest {
          src = workspace.stagedSrc;
          inherit (workspace) cargoArtifacts;
          patches = loomPatches;
          nativeBuildInputs = [ pkgs.git ];
          inherit cargoNextestExtraArgs;
        };
      loomCli = mkLoom pkgs;
      linuxLoomCli = mkLoom linuxPkgs;
    in
    {
      _module.args = {
        inherit loomCli linuxLoomCli loomLib;
      };
      packages.loom = loomCli;
      checks = {
        loom-spawn-contract = loomContract "-p loom-driver -E 'test(wrix_launch_) | test(wrix_runner_) | test(profile_manifest) | test(resolve_launcher)'";
        loom-runner-inputs = loomContract "-p loom-gate -E 'test(inputs::tests)'";
      };
    };
}
