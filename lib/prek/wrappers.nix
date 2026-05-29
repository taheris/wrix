{ pkgs }:

{
  prePushChecks = pkgs.writeShellScriptBin "pre-push-checks" (
    builtins.readFile ./wrappers/pre-push-checks.sh
  );

  skipIfMissing = pkgs.writeShellScriptBin "skip-if-missing" (
    builtins.readFile ./wrappers/skip-if-missing.sh
  );
}
