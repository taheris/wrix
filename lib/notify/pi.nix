{ pkgs }:

pkgs.linkFarm "wrix-pi-notify" [
  {
    name = "extensions/wrix-notify.ts";
    path = ./pi-extension.ts;
  }
]
