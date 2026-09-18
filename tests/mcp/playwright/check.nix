{
  pkgs,
  system,
  linuxPkgs,
  crane,
  fenix,
  treefmt,
  serviceCli,
}:

let
  inherit (pkgs.lib) escapeShellArg;

  imageProfiles = import ../../../lib/sandbox/profiles.nix {
    pkgs = linuxPkgs;
    hostPkgs = linuxPkgs;
    inherit crane fenix treefmt;
  };
  registry = import ../../../lib/mcp {
    pkgs = linuxPkgs;
    rustProfile = imageProfiles.rust;
  };
  server = registry.playwright;
  sandboxLib = import ../../../lib/sandbox {
    inherit
      pkgs
      system
      linuxPkgs
      crane
      fenix
      treefmt
      serviceCli
      ;
  };

  options = {
    headless = true;
    viewport = {
      width = 1280;
      height = 720;
    };
    config = { };
  };
  mandatoryOptions = options // {
    headless = false;
    config = {
      launchOptions = {
        args = [ "--config-arg" ];
        channel = "chrome";
        executablePath = "/bad/top";
        headless = true;
      };
      browser.launchOptions = {
        args = [ "--browser-arg" ];
        channel = "chrome";
        executablePath = "/bad/browser";
        headless = true;
      };
    };
  };
  userOptions = options // {
    headless = false;
    viewport = {
      width = 1440;
      height = 900;
    };
    config = {
      browser = {
        browserName = "firefox";
        launchOptions.slowMo = 0;
        contextOptions = {
          acceptDownloads = false;
          viewport = {
            width = 1;
            height = 1;
          };
        };
        userDataDir = "/tmp/wrix-playwright-user-data";
      };
      timeouts.action = 4321;
    };
  };

  mandatoryConfig = server.passthru.mkConfig mandatoryOptions;
  userConfig = server.passthru.mkConfig userOptions;
  generatedConfig = server.passthru.mkConfig options;
  serverConfig = server.mkServerConfig options;

  sandbox = sandboxLib.mkSandbox {
    profile = sandboxLib.profiles.base;
    agent = "claude";
    mcp.playwright = options;
  };

  mkCheck =
    name: nativeBuildInputs: check:
    pkgs.runCommand name { inherit nativeBuildInputs; } ''
      set -euo pipefail
      ${check}
      mkdir -p "$out/bin"
      cat > "$out/bin/${name}" <<'SCRIPT'
      #!${pkgs.runtimeShell}
      set -euo pipefail
      printf '%s\n' 'PASS: ${name}'
      SCRIPT
      chmod +x "$out/bin/${name}"
    '';
  mkEvaluationCheck =
    name: condition:
    assert condition;
    mkCheck name [ ] "";

  expectedFlags = [
    "--no-sandbox"
    "--disable-dev-shm-usage"
    "--disable-gpu"
  ];
  mandatoryFlagsCheck = mkEvaluationCheck "test-playwright-mandatory-flags" (
    mandatoryConfig.browser.launchOptions.args == expectedFlags
    ++ [
      "--config-arg"
      "--browser-arg"
    ]
    && mandatoryConfig.browser.launchOptions.channel == "chromium"
    && mandatoryConfig.browser.launchOptions.headless == false
    &&
      mandatoryConfig.browser.launchOptions.executablePath
      == "${server.passthru.chromiumExecutable}/bin/chrome"
  );
  userOptionsCheck = mkEvaluationCheck "test-playwright-user-options-config" (
    userConfig.browser.browserName == "chromium"
    && userConfig.browser.userDataDir == "/tmp/wrix-playwright-user-data"
    && userConfig.browser.launchOptions.headless == false
    && userConfig.browser.launchOptions.slowMo == 0
    &&
      userConfig.browser.contextOptions.viewport == {
        width = 1440;
        height = 900;
      }
    && userConfig.browser.contextOptions.acceptDownloads == false
    && !(userConfig ? contextOptions)
    && userConfig.timeouts.action == 4321
  );

  executablePathCheck = mkCheck "test-playwright-chromium-executable-path" [ pkgs.coreutils ] ''
    configured=${escapeShellArg generatedConfig.browser.launchOptions.executablePath}
    expected=${escapeShellArg "${server.passthru.chromiumExecutable}/bin/chrome"}
    if [[ "$configured" != "$expected" || ! -x "$configured" ]]; then
      printf 'configured Chromium helper is not executable: %s\n' "$configured" >&2
      exit 1
    fi
    link_target=$(readlink "$configured")
    case "$link_target" in
      ${linuxPkgs.playwright-driver.browsers}/chromium-*/chrome-linux/chrome | ${linuxPkgs.playwright-driver.browsers}/chromium-*/chrome-linux64/chrome) ;;
      *)
        printf 'Chromium helper link is outside playwright-driver.browsers: %s\n' "$link_target" >&2
        exit 1
        ;;
    esac
    target=$(readlink -f "$configured")
    if [[ ! -x "$target" ]]; then
      printf 'Chromium helper does not resolve to an executable: %s\n' "$target" >&2
      exit 1
    fi
  '';

  chromiumImageCheck =
    mkCheck "test-playwright-chromium-closure"
      [
        pkgs.coreutils
        pkgs.gnugrep
        pkgs.gnused
        pkgs.gnutar
        pkgs.gzip
        pkgs.jq
      ]
      ''
        tmp=$(mktemp -d)
        trap 'rm -rf "$tmp"' EXIT
        layer_paths="$tmp/layers"

        ${
          if pkgs.stdenv.hostPlatform.isLinux then
            ''
              descriptor=${escapeShellArg (toString sandbox.image.source)}
              layout=$(jq -er '.oci_layout' "$descriptor")
              jq -r --arg layout "$layout" '.layers[].digest | sub("^sha256:"; "") | $layout + "/blobs/sha256/" + .' \
                "$descriptor" > "$layer_paths"
            ''
          else
            ''
              archive="$tmp/image.tar"
              cp ${escapeShellArg (toString sandbox.image.source)} "$archive"
              mkdir "$tmp/archive"
              tar -xf "$archive" -C "$tmp/archive"
              jq -r --arg root "$tmp/archive" '.[0].Layers[] | $root + "/" + .' \
                "$tmp/archive/manifest.json" > "$layer_paths"
            ''
        }

        member_exists() {
          local member="$1"
          local layer
          while IFS= read -r layer; do
            if tar -tf "$layer" | sed 's#^\./##; s#^/##' | grep -Fx "$member" >/dev/null; then
              return 0
            fi
          done < "$layer_paths"
          return 1
        }

        helper=${escapeShellArg "${server.passthru.chromiumExecutable}/bin/chrome"}
        target=$(readlink -f "$helper")
        helper_member="''${helper#/}"
        target_member="''${target#/}"

        if ! member_exists "$helper_member"; then
          printf 'assembled image layers do not contain Chromium helper %s\n' "$helper" >&2
          exit 1
        fi
        if ! member_exists "$target_member"; then
          printf 'assembled image layers do not contain packaged Chromium binary %s\n' "$target" >&2
          exit 1
        fi
      '';

  registryPackagesComplete = builtins.all (package: builtins.elem package server.packages) [
    linuxPkgs.playwright-mcp
    linuxPkgs.playwright-driver.browsers
    server.passthru.chromiumExecutable
  ];
  registryPackagesIncluded = builtins.all (
    package: builtins.elem package sandbox.profile.packages
  ) server.packages;
  registryTripleCheck =
    assert server.name == "playwright";
    assert builtins.isFunction server.mkServerConfig;
    assert registryPackagesComplete;
    assert registryPackagesIncluded;
    mkEvaluationCheck "test-playwright-registry-triple" (
      serverConfig.command == "playwright-mcp"
      && builtins.elemAt serverConfig.args 0 == "--config"
      && serverConfig.env.PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD == "1"
    );
in
{
  inherit
    chromiumImageCheck
    executablePathCheck
    mandatoryFlagsCheck
    registryTripleCheck
    sandbox
    userOptionsCheck
    ;
}
