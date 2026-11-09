{ pkgs }:
let
  initializer = import ./host-hook-store.nix { inherit pkgs; };
in
pkgs.runCommand "host-hook-store-offline-fixture" { nativeBuildInputs = [ pkgs.nix ]; } ''
  export HOME="$TMPDIR/home"
  mkdir -p "$HOME"
  if nix-store --store "$TMPDIR/empty" --option substituters "" --realise ${pkgs.bash} > empty.stdout 2> empty.stderr; then
    echo 'an unseeded private store unexpectedly realised the interpreter' >&2
    exit 1
  fi
  interpreter=$(${initializer}/bin/wrix-test-host-hook-store-init "$TMPDIR/seeded")
  test "$interpreter" = '${pkgs.bash}/bin/bash'
  test -x "$interpreter"
  nix-store --store "$TMPDIR/seeded" --option substituters "" --realise ${pkgs.bash} > /dev/null
  nix-store --store "$TMPDIR/seeded" --verify --check-contents > /dev/null
  touch "$out"
''
