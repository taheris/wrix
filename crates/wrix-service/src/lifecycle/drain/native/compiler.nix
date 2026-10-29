{ pkgs }:

let
  compiler = pkgs.callPackage ./sccache.nix { };
in
assert compiler.version == "0.18.0";
compiler.overrideAttrs (old: {
  patches = (old.patches or [ ]) ++ [ ./compiler.patch ];
  postPatch = (old.postPatch or "") + ''
    cp ${./compiler.rs} src/work_drain.rs
    cp ${../../../../../../lib/services/celler-work-drain-state.rs} src/work_drain_state.rs
  '';
})
