{
  pkgs,
  wrix,
  src,
}:
let
  inherit (builtins) fromJSON readFile unsafeDiscardStringContext;
  inherit (pkgs.lib) getExe last;
  pi = import ../../lib/sandbox/pi.nix { inherit pkgs; };
  runner = import ./fixtures/command-runner.nix { inherit pkgs; };
  inherit ((wrix.mkSandbox { })) image;
  stream = image.stream or image;
  customLayer = last stream.conf.drvAttrs.layersJsonFile.drvAttrs.exclude_paths;
  manifestFor =
    agent: runtime:
    (wrix.mkSandbox {
      inherit agent;
      agentPkg = if agent == "direct" then runner else null;
      mcpRuntime = runtime;
      mcp = if runtime then { } else { playwright = { }; };
    }).image.mcpAvailableJson;
  parsedManifest =
    agent: selection: fromJSON (unsafeDiscardStringContext (readFile (manifestFor agent selection)));
  explicit = parsedManifest "pi" false;
  runtime = parsedManifest "pi" true;
  nativeCheck =
    pkgs.runCommand "test-pi-mcp-native"
      {
        nativeBuildInputs = [
          pi
          pkgs.nodejs
          pkgs.bash
          pkgs.coreutils
          pkgs.gnused
          pkgs.jq
          pkgs.git
          pkgs.gnugrep
        ];
      }
      ''
        set -euo pipefail
        export REPO_ROOT=${src}
        export PI_TEST_BASH=${getExe pkgs.bash}
        export PI_TEST_PACKAGE=${pi}/lib/node_modules/pi-monorepo
        export PI_TEST_ENTRYPOINT=${./pi-mcp-entrypoint.sh}
        export PI_TEST_MCP_FIXTURE=${./fixtures/mcp-server.mjs}
        export PI_TEST_MODEL_FIXTURE=${./fixtures/model-server.mjs}
        export PI_TEST_SETTINGS=${(wrix.mkSandbox { }).image.piSettingsJson}
        bash "$REPO_ROOT/tests/sandbox/entrypoint-contract.sh" test_runtime_fixtures_do_not_need_env_or_path
        node --test ${./pi-mcp-native.mjs}
        touch "$out"
      '';
in
{
  checks = {
    mcp-manifest-handoff =
      assert explicit.schema == 1 && explicit.runtime_selection == false;
      assert explicit == parsedManifest "claude" false;
      assert explicit == parsedManifest "direct" false;
      assert runtime.schema == 1 && runtime.runtime_selection == true;
      assert runtime == parsedManifest "claude" true;
      assert runtime == parsedManifest "direct" true;
      assert builtins.elem (builtins.head explicit.servers) runtime.servers;
      nativeCheck;
    pi-mcp-native = nativeCheck;
  };
  imageWiring = pkgs.writeShellApplication {
    name = "test-pi-mcp-image-wiring";
    runtimeInputs = [
      pkgs.gnutar
      pkgs.findutils
      pkgs.coreutils
    ];
    text = ''
      set -euo pipefail
      image_dir=$(mktemp -d)
      trap 'rm -rf "$image_dir"' EXIT
      tar -xf ${customLayer}/layer.tar -C "$image_dir" ./etc/wrix/pi-agent ./etc/wrix/mcp-available.json
      [[ "$(find "$image_dir/etc/wrix/pi-agent/extensions" -mindepth 1 -printf '%f')" == wrix-notify.ts ]]
      [[ -f "$image_dir/etc/wrix/pi-agent/settings.json" ]]
      [[ -f "$image_dir/etc/wrix/mcp-available.json" ]]
      echo "PASS: baked Pi configuration retains notifications and contains no custom MCP extension"
    '';
  };
}
