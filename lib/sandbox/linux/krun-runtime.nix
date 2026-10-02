{ linuxPkgs }:

linuxPkgs.crun.overrideAttrs (old: {
  pname = "crun-krun";
  patches = (old.patches or [ ]) ++ [ ./crun-ring-buffer-pipe-capacity.patch ];
  buildInputs = old.buildInputs ++ [ linuxPkgs.libkrun ];
  configureFlags = (old.configureFlags or [ ]) ++ [ "--with-libkrun" ];
  postFixup = (old.postFixup or "") + ''
    patchelf --add-rpath ${linuxPkgs.lib.getLib linuxPkgs.libkrun}/lib "$out/bin/crun"
  '';
})
