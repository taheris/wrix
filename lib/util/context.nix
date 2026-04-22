# Context pinning utility for AI agents
#
# Provides a function to read the project pin file (default docs/README.md)
# for better AI search hit rates. Used by sandbox entrypoints and ralph commands.
{ pkgs }:

let
  inherit (pkgs) writeShellScriptBin;
in
{
  # Script that outputs pinned project context
  pin-context = writeShellScriptBin "pin-context" ''
    pin_file="''${1:-docs/README.md}"
    if [ -f "$pin_file" ]; then
      echo "Context pinned: $pin_file" >&2
      cat "$pin_file"
    else
      echo "No pinned context file found at $pin_file" >&2
      echo ""
    fi
  '';
}
