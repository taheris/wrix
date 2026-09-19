{ pkgs }:
let
  pi = import ../../lib/sandbox/pi.nix { inherit pkgs; };
  forwarded = platform: (import ../../lib/sandbox/${platform} { serviceCli = pi; }).mkSandbox { };
in
assert forwarded "linux" == pi;
assert forwarded "darwin" == pi;
pkgs.runCommand "test-pi-auth-storage" { nativeBuildInputs = [ pkgs.nodejs ]; } ''
  export PI_AUTH_TEST_PACKAGE=${pi}/lib/node_modules/pi-monorepo
  node ${./pi-auth-storage.mjs}
  touch "$out"
''
