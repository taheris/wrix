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
          cargoToml = inputs.loom-src + "/Cargo.toml";
          cargoLock = inputs.loom-src + "/Cargo.lock";
          cargoVendorDir = workspace.bin.cargoVendorDir;
          inherit (workspace) cargoArtifacts;
          doInstallCargoArtifacts = false;
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
      checks =
        let
          contracts = {
            loom-spawn-contract = loomContract "-p loom-driver -E 'test(wrix_launch_) | test(wrix_runner_) | test(profile_manifest) | test(resolve_launcher) | test(consumer_pin_)'";
            loom-runner-inputs = loomContract "-p loom-gate -E 'test(inputs::tests)'";
          };
          canonicalInputs =
            contract:
            !contract.doInstallCargoArtifacts
            && contract.cargoToml == inputs.loom-src + "/Cargo.toml"
            && contract.cargoLock == inputs.loom-src + "/Cargo.lock"
            && toString contract.cargoVendorDir == toString loomCli.cargoVendorDir;
        in
        contracts
        // {
          loom-contract-metadata =
            assert builtins.all canonicalInputs (builtins.attrValues contracts);
            pkgs.runCommand "loom-contract-metadata" { } ''touch "$out"'';
        };
    };
}
