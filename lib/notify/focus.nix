{ pkgs, binary }:

pkgs.runCommand "wrix-focus-notification"
  {
    nativeBuildInputs = [ pkgs.makeBinaryWrapper ];
    meta.mainProgram = "wrix-focus-notification";
  }
  ''
    mkdir -p "$out/bin"
    makeWrapper ${binary}/bin/wrix-focus-notification "$out/bin/wrix-focus-notification" \
      --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.tmux ]}
  ''
