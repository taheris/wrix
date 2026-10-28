{ linuxPkgs }:

let
  # Standard upstream virtio-net support; the firmware/kernel is unchanged.
  libkrun = linuxPkgs.libkrun.override { withNet = true; };
in
(linuxPkgs.crun.override { inherit libkrun; }).overrideAttrs (old: {
  pname = "crun-krun";
  patches = (old.patches or [ ]) ++ [ ./crun-ring-buffer-pipe-capacity.patch ];
  buildInputs = old.buildInputs ++ [ libkrun ];
  postPatch = (old.postPatch or "") + ''
    substituteInPlace src/libcrun/handlers/krun.c \
      --replace-fail '"/usr/bin/passt"' '"${linuxPkgs.passt}/bin/passt"'
  '';
  configureFlags = (old.configureFlags or [ ]) ++ [ "--with-libkrun" ];
  postFixup = (old.postFixup or "") + ''
    patchelf --add-rpath ${linuxPkgs.lib.getLib libkrun}/lib "$out/bin/crun"
  '';
})
