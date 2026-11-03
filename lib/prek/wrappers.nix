{ pkgs }:

{
  prePushChecks = pkgs.writeShellScriptBin "pre-push-checks" (
    builtins.readFile ../../bin/pre-push-checks
  );
}
