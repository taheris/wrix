{ pkgs }:

pkgs.writeShellApplication {
  name = "wrix-focus-notification";
  runtimeInputs = [ pkgs.tmux ];
  text = ''
    exec ${pkgs.python3}/bin/python3 ${./attention.py} "$@"
  '';
}
