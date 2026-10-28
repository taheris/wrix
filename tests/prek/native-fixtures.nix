{
  pkgs,
  packagedWrix,
  celler,
  devshellHook ? null,
  controller ? null,
  serviceImage ? null,
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
    WRIX_TEST_SCCACHE = "${
      import ../../crates/wrix-service/src/lifecycle/drain/native/compiler.nix { inherit pkgs; }
    }/bin/sccache";
    WRIX_TEST_SQL_CLIENT = pkgs.writeScript "native-sql-wire-client" ''
      #!${pkgs.python3}/bin/python3
      ${builtins.readFile ../../crates/wrix-cli/tests/fixtures/sql-client.py}
    '';
  }
  // optionalAttrs (controller != null && serviceImage != null) {
    WRIX_TEST_CONTROLLER_BINDING = import ../../lib/services/control-binding.nix {
      inherit pkgs controller;
      runtime = import ./controller-runtime.nix { inherit pkgs; };
      image = serviceImage;
    };
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
      {
        name = "compiler";
        path = environment.WRIX_TEST_SCCACHE;
      }
      {
        name = "sql-client";
        path = environment.WRIX_TEST_SQL_CLIENT;
      }
    ]
    ++ optionals (controller != null && serviceImage != null) [
      {
        name = "controller-binding";
        path = environment.WRIX_TEST_CONTROLLER_BINDING;
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
