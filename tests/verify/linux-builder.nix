{ pkgs, ... }:

let
  inherit (pkgs.lib) escapeShellArg;

  builderScript = function: ''
    run_repo_script ${escapeShellArg "tests/builder/key-material.sh"} ${escapeShellArg function}
  '';
in
{
  "linux-builder.integration" = ''
    run_repo_script ${escapeShellArg "tests/standalone/builder-test.sh"}
  '';

  "linux-builder.key-material-generation" = builderScript "test_generates_per_user_ed25519_material";

  "linux-builder.key-material-idempotent" = builderScript "test_preserves_existing_private_keys";

}
