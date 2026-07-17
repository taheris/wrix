{ pkgs }:

{
  prePushChecks = pkgs.writeShellScriptBin "pre-push-checks" (
    builtins.readFile ../../bin/pre-push-checks
  );

  skipIfMissing = pkgs.writeShellScriptBin "skip-if-missing" (
    builtins.readFile ./wrappers/skip-if-missing.sh
  );
}
