{ pkgs, craneLib }:

let
  # Evaluator inputs stay immutable paths; runtime modifications happen only at build time.
  upstream = builtins.fetchTarball {
    url = "https://github.com/celler-cache/celler/archive/de7e34848770f3cd274a276e15d3c459fee86f99.tar.gz";
    sha256 = "sha256-qDnTXcnV7ZvmGMtpEse+ymr05QuuULtlzroqGrpP8VA=";
  };
  runtimeArgs = {
    patches = [
      ./celler-ensure-key.patch
      ./celler-work-drain.patch
    ];
    postPatch = ''
      cp ${./celler-work-drain.rs} server/src/work_drain.rs
      cp ${./celler-work-drain-state.rs} server/src/work_drain_state.rs
      cp ${./celler-work-drain-transport.rs} server/src/work_drain_transport.rs
    '';
  };
  withRuntime =
    args:
    args
    // {
      patches = (args.patches or [ ]) ++ runtimeArgs.patches;
      postPatch = (args.postPatch or "") + runtimeArgs.postPatch;
    };
in
(pkgs.callPackage "${upstream}/crane.nix" {
  craneLib = craneLib // {
    buildPackage = args: craneLib.buildPackage (withRuntime args);
  };
}).celler
