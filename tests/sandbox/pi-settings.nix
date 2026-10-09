{ pkgs, wrix }:
let
  inherit (builtins)
    all
    attrNames
    fromJSON
    readFile
    removeAttrs
    ;
  settingsFile = agentSettings: (wrix.mkSandbox { inherit agentSettings; }).image.piSettingsJson;
  defaultsFile = settingsFile { };
  defaults = fromJSON (readFile defaultsFile);
  consumerSettings = {
    defaultProvider = "consumer-provider";
    defaultModel = "consumer-model";
    defaultThinkingLevel = "low";
    tuiMode = "fullscreen";
    editorPaddingX = 3;
    theme = "dark";
    defaultTools = [ "read" ];
    codemode.mode = "only";
  };
  overridden = fromJSON (readFile (settingsFile consumerSettings));
  budgetSettings = fromJSON (
    readFile (settingsFile {
      codemode.inlineBudget = 42;
    })
  );
  disabledFile = settingsFile { defaultTools = [ "-codemode" ]; };
  disabled = fromJSON (readFile disabledFile);
  pi = import ../../lib/sandbox/pi.nix { inherit pkgs; };
in
{
  pi-settings-precedence =
    assert defaults.defaultTools == [ "+codemode" ];
    assert defaults.codemode.mode == "on";
    assert defaults.defaultProjectTrust == "always";
    assert defaults.enableInstallTelemetry == false;
    assert defaults.sessionDir == "/workspace/.pi/agent/sessions";
    assert all (name: overridden.${name} == consumerSettings.${name}) (attrNames consumerSettings);
    assert
      removeAttrs overridden (attrNames consumerSettings)
      == removeAttrs defaults (attrNames consumerSettings);
    assert
      budgetSettings == defaults
      // {
        codemode = defaults.codemode // {
          inlineBudget = 42;
        };
      };
    assert disabled == defaults // { defaultTools = [ "-codemode" ]; };
    pkgs.runCommand "test-pi-settings-precedence" { } ''
      set -euo pipefail
      touch "$out"
    '';

  pi-codemode-tools =
    pkgs.runCommand "test-pi-codemode-tools" { nativeBuildInputs = [ pkgs.nodejs ]; }
      ''
        set -euo pipefail
        export PI_TEST_PACKAGE=${pi}/lib/node_modules/pi-monorepo
        export PI_TEST_BIN=${pi}/bin/pi
        export PI_TEST_MODEL_FIXTURE=${./fixtures/model-server.mjs}
        export PI_TEST_DEFAULT_SETTINGS=${defaultsFile}
        export PI_TEST_DISABLED_SETTINGS=${disabledFile}
        node --test ${./pi-codemode-tools.mjs}
        touch "$out"
      '';
}
