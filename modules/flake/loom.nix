{ inputs, ... }:

{
  perSystem =
    { pkgs, linuxPkgs, ... }:
    let
      loomLib = import "${inputs.loom-src}/nix/lib.nix";
      # Upstream source bindings belong to Loom, not this consumer checkout.
      external = artifact: builtins.removeAttrs artifact [ "loom" ];
      loomWorkspace =
        pkgs:
        loomLib.mkLoom {
          inherit pkgs;
          inherit (inputs) crane fenix;
          src = inputs.loom-src;
          cargoToml = inputs.loom-src + "/Cargo.toml";
          cargoLock = inputs.loom-src + "/Cargo.lock";
        };
      mkLoom =
        {
          pkgs,
          directRuntime ? false,
        }:
        external (
          (loomWorkspace pkgs).bin.overrideAttrs (old: {
            postInstall = (old.postInstall or "") + ''
              find "$out/bin" -type f ! -name loom ! -name loom-walk ${pkgs.lib.optionalString directRuntime "! -name loom-direct-runner"} -delete
            '';
          })
        );
      loomContract =
        cargoNextestExtraArgs:
        external (
          (loomWorkspace pkgs).mkCheck {
            doInstallCargoArtifacts = false;
            inherit cargoNextestExtraArgs;
          }
        );
      loomCli = mkLoom { inherit pkgs; };
      linuxLoomCli = mkLoom {
        pkgs = linuxPkgs;
        directRuntime = true;
      };
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
