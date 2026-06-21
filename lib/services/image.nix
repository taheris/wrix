{ pkgs, cacheServe }:

let
  labels = {
    "wrix.managed" = "true";
    "wrix.image.kind" = "service";
  };
in
pkgs.dockerTools.buildLayeredImage {
  name = "wrix-service";
  tag = "latest";

  contents = [
    pkgs.dockerTools.binSh
    pkgs.dockerTools.usrBinEnv
    pkgs.coreutils
    pkgs.dolt
    cacheServe
  ];

  extraCommands = ''
    mkdir -p tmp
    chmod 1777 tmp
  '';

  config = {
    Env = [
      "HOME=/tmp"
      "PATH=/bin:/usr/bin"
    ];
    Labels = labels;
  };
}
// {
  inherit labels;
}
