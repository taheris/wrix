{ pkgs, wrix }:
let
  checkMode =
    name: agentSettings: mode:
    let
      inherit
        (
          (wrix.mkSandbox {
            agent = "pi";
            inherit agentSettings;
          })
        )
        image
        ;
    in
    pkgs.runCommand name { nativeBuildInputs = [ pkgs.jq ]; } ''
      set -euo pipefail
      jq -e --arg mode "${mode}" '.tuiMode == $mode' ${image.piSettingsJson}
      touch "$out"
    '';
in
{
  pi-tui-mode-default = checkMode "test-pi-tui-mode-default" { } "regular";
  pi-tui-mode-inherited = checkMode "test-pi-tui-mode-inherited" { editorPaddingX = 2; } "regular";
  pi-tui-mode-override = checkMode "test-pi-tui-mode-override" {
    tuiMode = "fullscreen";
  } "fullscreen";
}
