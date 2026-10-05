let
  linux = [
    "aarch64-linux"
    "x86_64-linux"
  ];
  darwin = [
    "aarch64-darwin"
    "x86_64-darwin"
  ];
  native = linux ++ darwin;
  requiring = platforms: capabilities: script: { inherit script platforms capabilities; };
in
{
  inherit
    linux
    darwin
    native
    requiring
    ;
  live = requiring native [ "container-runtime" ];
  linuxLive = requiring linux [ "container-runtime" ];
  darwinLive = requiring darwin [ "container-runtime" ];
}
