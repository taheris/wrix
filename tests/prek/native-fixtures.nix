{
  pkgs,
  packagedWrix,
  celler,
  devshellHook ? null,
}:

let
  inherit (pkgs.lib) optionalAttrs optionals;
  environment = {
    WRIX_TEST_PREK_HOOKS = import ../../lib/prek/bundle.nix { inherit pkgs; };
    WRIX_TEST_PREK_RUNNER = "${import ../../lib/prek/runner.nix { inherit pkgs; }}/bin/wrix-prek";
    WRIX_TEST_BROKEN_PREK_RUNNER = "${import ./missing-runtime.nix { inherit pkgs; }}/bin/wrix-prek";
    WRIX_TEST_PREK_BUILD_SPEC = import ./bundle-build-spec.nix { inherit pkgs; };
    WRIX_TEST_PACKAGED_WRIX = "${packagedWrix}/bin/wrix";
    WRIX_TEST_CELLERD = "${celler}/bin/cellerd";
  }
  // optionalAttrs (devshellHook != null) {
    WRIX_TEST_DEVSHELL_HOOK = pkgs.writeText "wrix-test-devshell-hook" devshellHook;
  };
in
{
  inherit environment;
  bundle = pkgs.linkFarm "wrix-native-test-fixtures" (
    [
      {
        name = "hooks";
        path = environment.WRIX_TEST_PREK_HOOKS;
      }
      {
        name = "runner";
        path = environment.WRIX_TEST_PREK_RUNNER;
      }
      {
        name = "broken-runner";
        path = environment.WRIX_TEST_BROKEN_PREK_RUNNER;
      }
      {
        name = "builder-spec";
        path = environment.WRIX_TEST_PREK_BUILD_SPEC;
      }
      {
        name = "packaged-wrix";
        path = environment.WRIX_TEST_PACKAGED_WRIX;
      }
      {
        name = "cellerd";
        path = environment.WRIX_TEST_CELLERD;
      }
    ]
    ++ optionals (devshellHook != null) [
      {
        name = "devshell-hook";
        path = environment.WRIX_TEST_DEVSHELL_HOOK;
      }
    ]
  );
}
