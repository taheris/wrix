# Test entry point - exports checks and test runner app
{
  pkgs,
  system,
  linuxPkgs,
  treefmt,
  src,
  wrix,
  crane,
  fenix,
  loomTests ? {
    checks = { };
    ciApps = [ ];
  },
}:

let
  inherit (pkgs.lib)
    concatStringsSep
    escapeShellArg
    makeBinPath
    optionalAttrs
    optionals
    removeAttrs
    ;
  inherit (pkgs)
    bash
    coreutils
    gawk
    git
    gnugrep
    gnused
    jq
    netcat
    nix
    socat
    writeShellScriptBin
    ;

  # ============================================================================
  # Pure Nix Checks (run via `nix flake check`)
  # ============================================================================

  # Smoke tests run on all platforms
  smokeTests = import ./sandbox/smoke.nix {
    inherit
      pkgs
      system
      treefmt
      crane
      fenix
      ;
    serviceCli = wrix.rustPackage.wrix;
  };

  # Test sandbox image with `hello` as a stand-in for claude/beads.
  # Exposed as `packages.test-image-base` so the host-side podman
  # verifiers can `nix build .#test-image-base` without rebuilding the
  # full claude/beads closure.
  #
  # `basePerturbed` is the same image with a one-attr claudeConfig
  # difference — the streamLayeredImage customisation layer's hash
  # changes while every base-layer blob remains identical. The
  # image-install-delta-bounded verifier installs both and asserts
  # the platform store only takes on bytes for the changed top layer.
  auditClock = import ./security/audit-clock.nix { inherit pkgs linuxPkgs wrix; };

  mkTestImage =
    args:
    import ./sandbox/test-image.nix (
      {
        pkgs = linuxPkgs;
        inherit treefmt;
        inherit (wrix) mkSandbox;
      }
      // args
    );

  testImages = {
    auditCollision = auditClock.image;
    gitCredentials =
      (wrix.mkSandbox {
        agent = "direct";
        agentPkg = import ./sandbox/fixtures/command-runner.nix { pkgs = linuxPkgs; };
      }).image;
    base = mkTestImage { };
    basePerturbed = mkTestImage {
      claudeConfig = {
        _wrix_delta_bounded_probe = "v2";
      };
    };
    baseDirect = mkTestImage {
      agent = "direct";
      agentPkg = import ./sandbox/fixtures/command-runner.nix { pkgs = linuxPkgs; };
    };
    basePi = mkTestImage {
      agent = "pi";
    };
    baseBeads = import ./sandbox/beads-test-image.nix {
      pkgs = linuxPkgs;
    };
    # Same image but with `pkgs.nix` added to the profile's packages — a
    # nix-shipping profile. Consumed by tests/sandbox/nix-in-container.sh,
    # which drives live `nix develop`/`nix build` as the unprivileged
    # runtime user and asserts no store-permission failure (FR #13).
    nix = mkTestImage {
      shipNix = true;
    };
  };

  utilityTests = import ./util/checks.nix { inherit pkgs; };
  builderRouteTest = import ./builder/vmnet-route.nix { inherit pkgs; };

  # Darwin mount tests run on all platforms (test logic, not VM)
  darwinMountTests = import ./darwin/mounts.nix { inherit pkgs treefmt; };

  # Darwin network tests run on all platforms (test logic, not VM)
  darwinNetworkTests = import ./darwin/network.nix { inherit pkgs treefmt; };

  # Darwin UID mapping tests (verify unshare-based VirtioFS ownership fix)
  darwinUidTests = import ./darwin/uid.nix { inherit pkgs treefmt; };

  tmuxTests = import ./tmux {
    inherit
      pkgs
      linuxPkgs
      wrix
      src
      ;
  };

  playwrightMcpTests = import ./mcp/playwright/check.nix {
    inherit
      pkgs
      system
      linuxPkgs
      crane
      fenix
      treefmt
      ;
    serviceCli = wrix.rustPackage.wrix;
  };

  piMcpTests = import ./sandbox/pi-mcp-native.nix { inherit pkgs wrix src; };

  # Profile-image runtime checks share a craneLib + linux-package set with
  # the standalone tests below. They verify the per-profile sandbox images
  # contain the expected agent runtime binary.
  sandboxImageChecks = import ./sandbox/image-checks.nix {
    inherit
      pkgs
      system
      linuxPkgs
      crane
      fenix
      treefmt
      ;
    serviceCli = wrix.rustPackage.wrix;
  };

  linuxBuilderChecks = import ./builder/checks.nix {
    inherit pkgs linuxPkgs;
  };

  rustChecks = {
    wrix-rust-clippy = wrix.rustPackage.clippy;
    wrix-rust-nextest = wrix.rustPackage.nextest;
  };

  verify = import ./verify { inherit pkgs system linuxPkgs; };

  systemTests = optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
    beads-live-system = import ./services/beads-system.nix {
      inherit pkgs wrix;
      beadsImage = testImages.baseBeads;
    };
    services-devshell-start-independent = import ./services/devshell-lifecycle.nix {
      inherit pkgs wrix;
    };
    services-limit-mode-cache-endpoint = import ./services/cache-network-system.nix {
      inherit pkgs wrix;
      sandboxImage = testImages.base;
    };
  };

  prePushSmokeTests = removeAttrs smokeTests [
    "builder-keys-structure"
    "image-builds"
    "linux-pasta-port-forwarding-disabled"
    "network-mode-configuration"
    "package-runtime-path"
    "package-service-image-contract"
    "package-script-syntax"
    "script-syntax"
  ];

  ciChecks = rustChecks // {
    inherit (smokeTests)
      builder-keys-structure
      image-builds
      linux-pasta-port-forwarding-disabled
      network-mode-configuration
      package-runtime-path
      package-service-image-contract
      package-script-syntax
      script-syntax
      ;
  };

  # README example verification
  readmeTest = {
    readme = import ./readme.nix {
      inherit pkgs src system;
    };
  };

  # All checks combined
  checks =
    darwinMountTests
    // darwinNetworkTests
    // darwinUidTests
    // readmeTest
    // utilityTests
    // prePushSmokeTests
    // tmuxTests.checks
    // (import ./sandbox/pi-settings.nix { inherit pkgs wrix; })
    // piMcpTests.checks
    // {
      builder-vmnet-route = builderRouteTest;
      rust-source-fixtures = import ./services/rust-source.nix {
        inherit pkgs;
        inherit (wrix) rustPackage;
      };
      image-assembly-native = sandboxImageChecks.imageAssemblyNativeCheck;
      audit-start-clock = auditClock.check;
      entrypoint-hook-failures =
        pkgs.runCommandLocal "entrypoint-hook-failures"
          {
            nativeBuildInputs = [
              bash
              coreutils
              git
              jq
              pkgs.python3
            ];
          }
          ''
            set -euo pipefail
            export REPO_ROOT=${src}
            ${pkgs.python3}/bin/python3 ${./sandbox/test_entrypoint_failures.py}
            touch "$out"
          '';
      pi-auth-storage = import ./security/pi-auth.nix { inherit pkgs; };
      profile-images-launcher = import ./profiles/manifest.nix { inherit pkgs wrix; };
      verifier-inputs = import ./lib/inputs-test.nix { inherit pkgs; };
    }
    // loomTests.checks
    // optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
      system-test-prerequisites = import ./services/vm-prerequisites.nix { inherit pkgs system; };
      crun-ring-buffer-constrained = import ./sandbox/crun-ring-buffer.nix { inherit linuxPkgs; };
    };

  # ============================================================================
  # Test Runner Apps
  # ============================================================================

  # Fast tests: nix flake check (lint, smoke, unit tests)
  testAll = writeShellScriptBin "test-all" ''
    set -euo pipefail
    exec ${pkgs.nix}/bin/nix flake check "$@"
  '';

  inherit (import ./lib/verifier.nix) linux native nixosVm;
  mkCiApp = package: executable: {
    name = executable;
    inherit package executable;
  };
  mkDefinedCiApp =
    { package, inputs }: executable: (mkCiApp package executable) // { inherit inputs; };
  mkLiveCiApp =
    package: executable:
    (mkCiApp package executable)
    // {
      platforms = native;
      capabilities = [ "container-runtime" ];
    };
  ciRequirements = pkgs.writeText "test-ci-requirements.json" (
    builtins.toJSON (
      builtins.listToAttrs (
        map (app: {
          inherit (app) name;
          value = {
            platforms = app.platforms or [ ];
            capabilities = app.capabilities or [ ];
          };
        }) ciApps
      )
    )
  );
  inputDefinition = import ./lib/inputs.nix { };
  ciInputDescriptions = pkgs.writeText "test-ci-inputs.json" (
    builtins.toJSON (
      inputDefinition.project (
        builtins.listToAttrs (
          map (app: {
            inherit (app) name;
            value = app;
          }) ciApps
        )
      )
    )
  );
  ciPreflightPath = makeBinPath (
    [
      jq
      coreutils
    ]
    ++ optionals pkgs.stdenv.hostPlatform.isLinux [
      pkgs.podman
      pkgs.util-linux
    ]
  );

  mkSystemTestCiApp =
    name: test:
    import ./lib/system-test.nix {
      inherit
        pkgs
        system
        name
        test
        ;
    };
  mkServiceCiApp =
    package: executable:
    (mkCiApp package executable)
    // (
      if pkgs.stdenv.hostPlatform.isLinux then
        nixosVm
      else
        {
          platforms = native;
          capabilities = [ "container-runtime" ];
        }
    );

  ciApps = loomTests.ciApps ++ [
    (mkCiApp piMcpTests.imageWiring "test-pi-mcp-image-wiring")
    (mkTmuxCiApp "workflow")
    (mkTmuxCiApp "exited-process")
    (mkTmuxCiApp "targeted-cleanup")
    (mkTmuxCiApp "container-cleanup")
    (mkCiApp sandboxImageChecks.imageInstallRealSkopeoTest "test-image-install-real-skopeo")
    (mkCiApp sandboxImageChecks.imageInstallDigestSkipTest "test-image-install-digest-skip")
    (mkCiApp sandboxImageChecks.digestMatchesStoredIdTest "test-image-digest-matches-stored-id")
    (mkCiApp sandboxImageChecks.linuxImageArchivelessSourceTest "test-linux-image-archiveless-source")
    (mkCiApp sandboxImageChecks.imageDigestNoTarTest "test-image-digest-no-tar")
    (mkCiApp sandboxImageChecks.imageTierGraphTest "test-image-tier-graph")
    (mkCiApp sandboxImageChecks.imageNixConfigTest "test-image-nix-config")
    (mkCiApp sandboxImageChecks.imageCaCertificatesTest "test-image-ca-certificates")
    (mkCiApp sandboxImageChecks.imageEntrypointCommandTest "test-image-entrypoint-command")
    (mkCiApp sandboxImageChecks.imageAgentMarkerTest "test-image-agent-marker")
    (mkCiApp sandboxImageChecks.profilePackagesBundledTest "test-profile-packages-bundled")
    (mkCiApp sandboxImageChecks.imageTierMembershipTest "test-image-tier-membership")
    (mkCiApp sandboxImageChecks.wrixImagesSourceKindTest "test-wrix-images-source-kind")
    (mkCiApp sandboxImageChecks.wrixImageLabelsTest "test-wrix-image-labels")
    (mkCiApp sandboxImageChecks.agentDeclaredDirectRunnerTest "test-agent-declared-direct-runner")
    (mkCiApp sandboxImageChecks.agentConsumerRuntimeClosuresTest "test-agent-consumer-runtime-closures")
    (mkCiApp sandboxImageChecks.agentClaudeRuntimeTest "test-agent-claude-runtime")
    (mkCiApp sandboxImageChecks.claudeRuntimeNoopTest "test-claude-runtime-noop")
    (mkCiApp sandboxImageChecks.prekHooksClosureTest "test-prek-hooks-closure")
    (mkCiApp sandboxImageChecks.baseImageUniversalTest "test-base-image-universal")
    (mkCiApp sandboxImageChecks.entrypointResolverBaseTest "test-entrypoint-resolver-base")
    (mkCiApp sandboxImageChecks.baseImageHashStableTest "test-base-image-hash-stable")
    (mkCiApp sandboxImageChecks.stableProfileHashStableTest "test-stable-profile-hash-stable")
    (mkCiApp sandboxImageChecks.stableProfileMembershipTest "test-stable-profile-membership")
    (mkCiApp sandboxImageChecks.pinnedToolchainStableTest "test-pinned-toolchain-stable-tier")
    (mkCiApp sandboxImageChecks.downstreamChangeLeafOnlyTest "test-downstream-change-leaf-only")
    (mkCiApp sandboxImageChecks.archivelessGeneratedChangeTest "test-archiveless-generated-change")
    (mkCiApp sandboxImageChecks.agentTierIsolatedTest "test-agent-tier-isolated")
    (mkCiApp sandboxImageChecks.agentExclusiveTest "test-agent-exclusive")
    (mkCiApp sandboxImageChecks.agentPkgThreadedTest "test-agent-pkg-threaded")
    (mkCiApp sandboxImageChecks.iterationCostBoundedTest "test-iteration-cost-bounded")
    (mkCiApp sandboxImageChecks.customisationLayerBoundedTest "test-customisation-layer-bounded")
    (mkCiApp sandboxImageChecks.imageNixDbConsistentTest "test-image-nix-db-consistent")
    (mkCiApp sandboxImageChecks.imageNixDbNoDanglingTest "test-image-nix-db-no-dangling")
    (mkDefinedCiApp linuxBuilderChecks.sshdHardeningTest "test-linux-builder-sshd-hardening")
    (mkDefinedCiApp linuxBuilderChecks.imageSourceKindTest "test-linux-builder-image-source-kind")
    (mkDefinedCiApp linuxBuilderChecks.sourceKindLoadTransportTest "test-linux-builder-source-kind-load-transport")
    (mkCiApp testProfilesBuildPackage "test-profiles-build-package")
    (mkCiApp testProfileImagesManifestShape "test-profile-images-manifest-shape")
    (mkCiApp testProfileConfigImageSourceKind "test-profile-config-image-source-kind")
    (mkCiApp testProfileConfigWrapper "test-profile-config-wrapper")
    (mkCiApp testContainerPreCommit "test-container-pre-commit")
    (mkCiApp testContainerPrePush "test-container-pre-push")
    (mkCiApp testWrixCliInProfile "test-wrix-cli-in-profile")
    (mkCiApp testSandboxAgentSettings "test-sandbox-agent-settings")
    (mkCiApp testPlaywrightChromiumClosure "test-playwright-chromium-closure")
    (mkCiApp testPlaywrightChromiumExecutablePath "test-playwright-chromium-executable-path")
    (mkCiApp testPlaywrightMandatoryFlags "test-playwright-mandatory-flags")
    (mkCiApp testPlaywrightUserOptionsConfig "test-playwright-user-options-config")
    (mkCiApp testBeadsLiveSystem "test-beads-live-system")
    (mkServiceCiApp testServicesDevshellStartIndependent "test-services-devshell-start-independent")
    (mkServiceCiApp testServicesLimitModeCacheEndpoint "test-services-limit-mode-cache-endpoint")
    (mkLiveCiApp testSecurityAuditTrailAnchor "test-security-audit-trail-anchor")
    (mkLiveCiApp testSecurityGitSshBootstrap "test-security-explicit-git-ssh-bootstrap")
    (mkLiveCiApp testSecurityHostContainerLoomGitHelper "test-security-host-container-loom-git-helper")
    ((mkCiApp testImageGitHelperParity "test-image-git-helper-parity") // { platforms = linux; })
    (mkLiveCiApp testSecurityNestedKeyPropagation "test-security-explicit-nested-key-grants")
    (mkLiveCiApp testSecurityGitGrantIsolation "test-security-git-grant-isolation")
    (mkLiveCiApp testSecuritySessionLocalSigning "test-security-session-local-signing")
    (mkLiveCiApp testSecurityPiAuthIsolation "test-security-pi-auth-isolation")
    (
      (mkLiveCiApp testSecurityProviderCredentialEnv "test-security-provider-credential-env")
      // {
        platforms = linux;
      }
    )
  ];

  ciAppNameLines = concatStringsSep "\n" (map (app: "      ${app.name}") ciApps);
  ciAppDerivations = builtins.listToAttrs (
    map (app: {
      inherit (app) name;
      value = app.package;
    }) ciApps
  );
  testAppDerivations = ciAppDerivations // {
    playwright-mcp-registry = playwrightMcpTests.registryTripleCheck;
    playwright-mcp-sandbox = playwrightMcpTests.sandbox.package;
    test-notify = testNotify;
  };

  testCi = writeShellScriptBin "test-ci" ''
        set -euo pipefail
        export PATH="${ciPreflightPath}:$PATH"
        source ${./lib/verifier.sh}
        source ${./lib/print-inputs.sh}

        ci_checks=(
          builder-keys-structure
          wrix-rust-clippy
          wrix-rust-nextest
          image-builds
          linux-pasta-port-forwarding-disabled
          network-mode-configuration
          package-runtime-path
          package-service-image-contract
          package-script-syntax
          script-syntax
        )
        ci_apps=(
    ${ciAppNameLines}
        )

        if [[ "''${1:-}" = "--list" ]]; then
          printf 'check %s\n' "''${ci_checks[@]}"
          printf 'app %s\n' "''${ci_apps[@]}"
          exit 0
        fi

        repo_root=$(${pkgs.git}/bin/git rev-parse --show-toplevel 2>/dev/null || pwd) # best-effort: allow running outside a git checkout.
        cd "$repo_root"

        is_ci_app() {
          local requested="$1"
          local app
          for app in "''${ci_apps[@]}"; do
            if [[ "$app" == "$requested" ]]; then
              return 0
            fi
          done
          return 1
        }

        if [[ "''${1:-}" = "--print-inputs" ]]; then
          shift
          for app in "$@"; do
            if ! is_ci_app "$app"; then
              printf 'Unknown test-ci app: %s\n' "$app" >&2
              exit 64
            fi
          done
          verifier_print_inputs ${ciInputDescriptions} "$@"
          exit 0
        fi

        batch_dir=""
        declare -A ci_runners=()
        trap 'if [[ -n "$batch_dir" ]]; then rm -rf "$batch_dir"; fi' EXIT

        prepare_ci_apps() {
          local app runner index
          local selected=() installables=() links=()
          local -A seen=()
          for app in "$@"; do
            if is_ci_app "$app" && [[ ! -v "seen[$app]" ]]; then
              selected+=("$app")
              installables+=(".#legacyPackages.${system}.ciApps.$app")
              seen["$app"]=1
            fi
          done
          if [[ "''${#selected[@]}" -lt 2 ]]; then
            return 0
          fi
          batch_dir=$(mktemp -d -t wrix-ci-runners.XXXXXX)
          if ! ${pkgs.nix}/bin/nix build --no-warn-dirty --out-link "$batch_dir/runner" "''${installables[@]}"; then
            return 0 # Preserve per-app build verdicts through isolated fallback builds.
          fi
          links=("$batch_dir"/runner*)
          if [[ "''${#links[@]}" -ne "''${#selected[@]}" ]]; then
            return 0 # Ambiguous aliases or multiple outputs retain the existing individual path.
          fi
          for index in "''${!selected[@]}"; do
            app="''${selected[$index]}"
            runner="$batch_dir/runner"
            if [[ "$index" -ne 0 ]]; then
              runner="$runner-$index"
            fi
            if [[ ! -x "$runner/bin/$app" ]]; then
              ci_runners=()
              return 0 # Missing executables retain the existing per-app failure behavior.
            fi
            ci_runners["$app"]="$(${coreutils}/bin/readlink "$runner")"
          done
        }

        resolve_ci_app() {
          local app="$1"
          if [[ -v "ci_runners[$app]" ]]; then
            printf '%s\n' "''${ci_runners[$app]}"
          else
            ${pkgs.nix}/bin/nix build --no-link --print-out-paths --no-warn-dirty ".#legacyPackages.${system}.ciApps.$app"
          fi
        }

        run_ci_app() {
          local app="$1" runner
          runner=$(resolve_ci_app "$app") || return 1
          "$runner/bin/$app"
        }

        run_ci_json_app() {
          local app="$1" runner requirements build_log evidence
          requirements=$(jq -c --arg target "$app" '.[$target]' ${ciRequirements})
          build_log=$(mktemp -t wrix-ci-build.XXXXXX)
          if runner=$(resolve_ci_app "$app" 2>"$build_log"); then
            rm -f "$build_log"
            verifier_run "$app" "$requirements" "$runner/bin/$app"
          else
            evidence=$(head -c 4000 "$build_log")
            cat "$build_log" >&2
            rm -f "$build_log"
            verifier_emit "$app" failed "CI app build failed: $evidence" "$(verifier_platform)" "$requirements"
            return 1
          fi
        }

        if [[ "''${1:-}" = "--json" ]]; then
          shift
          if [[ "$#" -eq 0 ]]; then
            echo "test-ci --json requires at least one CI app" >&2
            exit 64
          fi

          prepare_ci_apps "$@"
          json_failed=0
          json_skipped=0
          for app in "$@"; do
            if ! is_ci_app "$app"; then
              verifier_emit "$app" failed "unknown test-ci app" "$(verifier_platform)" '{"platforms":[],"capabilities":[]}'
              json_failed=$((json_failed + 1))
              continue
            fi

            if run_ci_json_app "$app"; then
              continue
            else
              status="$?"
            fi
            case "$status" in
              77) json_skipped=$((json_skipped + 1)) ;;
              *) json_failed=$((json_failed + 1)) ;;
            esac
          done
          verifier_batch_exit "$json_failed" "$json_skipped"
          exit 0
        fi

        failed=0
        skipped=0
        run_step() {
          local name="$1"
          shift
          echo "=== $name ==="
          if "$@"; then
            echo "PASS: $name"
          else
            local status="$?"
            if [[ "$status" -eq 77 ]]; then
              echo "SKIP: $name" >&2
              skipped=$((skipped + 1))
            else
              echo "FAIL: $name" >&2
              failed=$((failed + 1))
            fi
          fi
        }

        run_step "nix flake check" ${pkgs.nix}/bin/nix flake check --no-warn-dirty

        for check in "''${ci_checks[@]}"; do
          run_step "$check" ${pkgs.nix}/bin/nix build --no-link --no-warn-dirty ".#legacyPackages.${system}.ciChecks.$check"
        done

        for app in "''${ci_apps[@]}"; do
          run_step "$app" run_ci_app "$app"
        done

        verifier_batch_exit "$failed" "$skipped"
  '';

  # profiles.rust.buildPackage hash invariant verifies (specs/profiles.md).
  # Driven via `nix eval` against the live flake, so it runs outside the build
  # sandbox like the other tests/profiles/*.sh scripts. Wrapper resolves
  # REPO_ROOT from the caller's git toplevel and threads jq + nix onto PATH.
  testProfilesBuildPackage = writeShellScriptBin "test-profiles-build-package" ''
    set -euo pipefail
    : "''${REPO_ROOT:=$(${git}/bin/git -C "''${PWD}" rev-parse --show-toplevel)}"
    export REPO_ROOT
    export PATH="${jq}/bin:${git}/bin:${nix}/bin:$PATH"
    exec ${bash}/bin/bash "$REPO_ROOT/tests/profiles/build-package.sh" "$@"
  '';

  mkRepoScriptCiApp =
    {
      name,
      script,
      args,
      environment ? "",
    }:
    writeShellScriptBin name ''
      set -euo pipefail
      : "''${REPO_ROOT:=$(${git}/bin/git -C "''${PWD}" rev-parse --show-toplevel)}"
      export REPO_ROOT
      export PATH="${bash}/bin:${coreutils}/bin:${gawk}/bin:${git}/bin:${gnugrep}/bin:${gnused}/bin:${jq}/bin:${nix}/bin:$PATH"
      ${environment}
      exec ${bash}/bin/bash "$REPO_ROOT/${script}" ${concatStringsSep " " (map escapeShellArg args)}
    '';

  mkTmuxCiApp =
    suffix:
    let
      name = "test-tmux-cli-${suffix}";
      package = mkRepoScriptCiApp {
        inherit name;
        script = "tests/tmux/workflow.sh";
        args = [ "test_cli_${builtins.replaceStrings [ "-" ] [ "_" ] suffix}" ];
        environment = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
          export PATH="${
            makeBinPath [
              pkgs.podman
              pkgs.openssh
            ]
          }:$PATH"
        '';
      };
    in
    (mkCiApp package name)
    // {
      platforms = linux;
      capabilities = [ "container-runtime" ];
    };

  testProfileImagesManifestShape = mkRepoScriptCiApp {
    name = "test-profile-images-manifest-shape";
    script = "tests/profiles/profile-images-manifest.sh";
    args = [ "test_manifest_shape" ];
  };
  testProfileConfigImageSourceKind = mkRepoScriptCiApp {
    name = "test-profile-config-image-source-kind";
    script = "tests/sandbox/profile-config-wrapper.sh";
    args = [ "test_image_source_kind" ];
  };
  testProfileConfigWrapper = mkRepoScriptCiApp {
    name = "test-profile-config-wrapper";
    script = "tests/sandbox/profile-config-wrapper.sh";
    args = [ "test_profile_config_wrapper_contract" ];
  };

  profileContainerHookPath = makeBinPath (
    [
      pkgs.findutils
      pkgs.openssh
    ]
    ++ optionals pkgs.stdenv.hostPlatform.isLinux [
      pkgs.podman
      pkgs.shadow
      pkgs.skopeo
      pkgs.util-linux
    ]
  );
  mkProfileContainerHookCiApp =
    name: linuxScript: darwinFunction:
    mkRepoScriptCiApp {
      inherit name;
      script =
        if pkgs.stdenv.hostPlatform.isDarwin then
          "tests/sandbox/container-hooks-darwin.sh"
        else
          linuxScript;
      args = optionals pkgs.stdenv.hostPlatform.isDarwin [ darwinFunction ];
      environment = ''
        export PATH="${profileContainerHookPath}:$PATH"
      '';
    };
  testContainerPreCommit =
    mkProfileContainerHookCiApp "test-container-pre-commit" "tests/sandbox/container-pre-commit.sh"
      "test_pre_commit_fires_in_darwin_profile_container";
  testContainerPrePush =
    mkProfileContainerHookCiApp "test-container-pre-push" "tests/sandbox/container-pre-push.sh"
      "test_pre_push_fires_in_darwin_profile_container";

  testWrixCliInProfile = mkRepoScriptCiApp {
    name = "test-wrix-cli-in-profile";
    script = "tests/sandbox/custom-mounts-env.sh";
    args = [ "test_wrix_cli_added_to_sandbox_profile" ];
  };
  testSandboxAgentSettings = mkRepoScriptCiApp {
    name = "test-sandbox-agent-settings";
    script = "tests/sandbox/agent-settings.sh";
    args = [ ];
  };

  ciLinuxSystem =
    if system == "aarch64-darwin" then
      "aarch64-linux"
    else if system == "x86_64-darwin" then
      "x86_64-linux"
    else
      system;
  testPlaywrightChromiumClosure = playwrightMcpTests.chromiumImageCheck;
  testPlaywrightChromiumExecutablePath = playwrightMcpTests.executablePathCheck;
  testPlaywrightMandatoryFlags = playwrightMcpTests.mandatoryFlagsCheck;
  testPlaywrightUserOptionsConfig = playwrightMcpTests.userOptionsCheck;

  testBeadsLiveSystem = writeShellScriptBin "test-beads-live-system" ''
    set -euo pipefail
    : "''${REPO_ROOT:=$(${git}/bin/git -C "''${PWD}" rev-parse --show-toplevel)}"
    exec ${nix}/bin/nix build --no-link --no-warn-dirty \
      "$REPO_ROOT#legacyPackages.${ciLinuxSystem}.systemTests.beads-live-system"
  '';

  serviceCiPath = makeBinPath [
    pkgs.curl
    pkgs.openssh
    pkgs.python3
    wrix.rustPackage.wrix
  ];
  serviceCiEnvironment = ''
    export PATH="${serviceCiPath}:$PATH"
    export WRIX_TEST_WRIX_BIN=${escapeShellArg "${wrix.rustPackage.wrix}/bin/wrix"}
  '';
  testServicesDevshellStartIndependent =
    if pkgs.stdenv.hostPlatform.isLinux then
      mkSystemTestCiApp "test-services-devshell-start-independent" "services-devshell-start-independent"
    else
      mkRepoScriptCiApp {
        name = "test-services-devshell-start-independent";
        script = "tests/services/host-nix-config.sh";
        args = [ "test_mkdevshell_nix_cache" ];
        environment = serviceCiEnvironment;
      };
  testServicesLimitModeCacheEndpoint =
    if pkgs.stdenv.hostPlatform.isLinux then
      mkSystemTestCiApp "test-services-limit-mode-cache-endpoint" "services-limit-mode-cache-endpoint"
    else
      mkRepoScriptCiApp {
        name = "test-services-limit-mode-cache-endpoint";
        script = "tests/services/cache-network-live.sh";
        args = [ "test_limit_mode_cache_endpoint" ];
        environment = serviceCiEnvironment;
      };

  securityCiPath = makeBinPath (
    [
      pkgs.findutils
      pkgs.openssh
    ]
    ++ optionals pkgs.stdenv.hostPlatform.isLinux [
      pkgs.podman
      pkgs.skopeo
      pkgs.util-linux
    ]
  );
  securityCiEnvironment = ''
    export PATH="${securityCiPath}:$PATH"
  '';
  testSecurityAuditTrailAnchor = mkRepoScriptCiApp {
    name = "test-security-audit-trail-anchor";
    script = "tests/security/audit-trail-anchor.sh";
    args = [ ];
    environment = securityCiEnvironment + ''
      export WRIX_TEST_AUDIT_COLLISION_IMAGE_ATTR="legacyPackages.${system}.testFixtures.auditCollision.source"
    '';
  };
  testSecurityGitSshBootstrap = mkRepoScriptCiApp {
    name = "test-security-explicit-git-ssh-bootstrap";
    script = "tests/security/git-ssh-bootstrap.sh";
    args = [ "test_explicit_git_ssh_bootstrap" ];
    environment = securityCiEnvironment;
  };
  testSecurityHostContainerLoomGitHelper = mkRepoScriptCiApp {
    name = "test-security-host-container-loom-git-helper";
    script = "tests/security/git-ssh-bootstrap.sh";
    args = [ "test_host_container_and_loom_helper" ];
    environment = securityCiEnvironment;
  };
  testImageGitHelperParity = writeShellScriptBin "test-image-git-helper-parity" (
    if pkgs.stdenv.hostPlatform.isLinux then
      ''
        set -euo pipefail
        export PATH="${testImages.base.profileEnv}/bin"
        exec ${bash}/bin/bash ${./sandbox/git-helper-parity.sh}
      ''
    else
      ''
        set -euo pipefail
        echo "SKIP: Linux fixture executable conformance requires Linux" >&2
        exit 77
      ''
  );
  testSecurityNestedKeyPropagation = mkRepoScriptCiApp {
    name = "test-security-explicit-nested-key-grants";
    script = "tests/security/nested-key-propagation.sh";
    args = [ ];
    environment = securityCiEnvironment;
  };
  testSecurityGitGrantIsolation = mkRepoScriptCiApp {
    name = "test-security-git-grant-isolation";
    script = "tests/security/git-grants.sh";
    args = [ "test_git_grant_isolation" ];
    environment = securityCiEnvironment;
  };
  testSecuritySessionLocalSigning = mkRepoScriptCiApp {
    name = "test-security-session-local-signing";
    script = "tests/security/git-grants.sh";
    args = [ "test_session_local_signing" ];
    environment = securityCiEnvironment;
  };
  testSecurityPiAuthIsolation = mkRepoScriptCiApp {
    name = "test-security-pi-auth-isolation";
    script = "tests/security/pi-auth-isolation.sh";
    args = [ ];
    environment = securityCiEnvironment;
  };
  testSecurityProviderCredentialEnv = mkRepoScriptCiApp {
    name = "test-security-provider-credential-env";
    script = "tests/security/provider-credential-env.sh";
    args = [ ];
    environment = securityCiEnvironment;
  };

  notifyFixture = import ./standalone/notify-fixture.nix { inherit pkgs; };

  testNotify = writeShellScriptBin "test-notify" ''
    set -euo pipefail
    : "''${REPO_ROOT:=$(${git}/bin/git -C "''${PWD}" rev-parse --show-toplevel)}"
    export REPO_ROOT
    export PATH="${notifyFixture.client}/bin:${notifyFixture.daemon}/bin:${bash}/bin:${coreutils}/bin:${gawk}/bin:${git}/bin:${gnugrep}/bin:${gnused}/bin:${jq}/bin:${netcat}/bin:${nix}/bin:${pkgs.python3}/bin:${socat}/bin:$PATH"
    exec ${bash}/bin/bash "$REPO_ROOT/tests/standalone/notify-test.sh" "$@"
  '';

in
{
  # Checks for `nix flake check`
  inherit checks;

  # App for `nix run .#test` — fast checks (~10s)
  app = {
    meta.description = "Run fast tests: nix flake check (lint, smoke, unit)";
    type = "app";
    program = "${testAll}/bin/test-all";
  };

  # Individual test apps for selective running
  apps = {
    claude-runtime-noop = {
      meta.description = "Verify bundled Claude sandbox image closure contains claude-code";
      type = "app";
      program = "${sandboxImageChecks.claudeRuntimeNoopTest}/bin/test-claude-runtime-noop";
    };

    # Cross-platform verifier for the launcher's digest-preflight install skip.
    image-install-digest-skip = {
      meta.description = "Verify launcher digest-preflight short-circuits image install";
      type = "app";
      program = "${sandboxImageChecks.imageInstallDigestSkipTest}/bin/test-image-install-digest-skip";
    };

    image-install-real-skopeo = {
      meta.description = "Verify launcher image install against real packaged skopeo (Linux only)";
      type = "app";
      program = "${sandboxImageChecks.imageInstallRealSkopeoTest}/bin/test-image-install-real-skopeo";
    };

    image-digest-matches-stored-id = {
      meta.description = "Skip legacy docker-archive digest verifier for descriptor images.";
      type = "app";
      program = "${sandboxImageChecks.digestMatchesStoredIdTest}/bin/test-image-digest-matches-stored-id";
    };

    linux-image-archiveless-source = {
      meta.description = "Verify Linux profile images publish nix-descriptor sources.";
      type = "app";
      program = "${sandboxImageChecks.linuxImageArchivelessSourceTest}/bin/test-linux-image-archiveless-source";
    };

    image-digest-no-tar = {
      meta.description = "Verify Linux descriptor image digests do not depend on raw image archives.";
      type = "app";
      program = "${sandboxImageChecks.imageDigestNoTarTest}/bin/test-image-digest-no-tar";
    };

    image-tier-graph = {
      meta.description = "Verify profile images expose the base/stable/agent/leaf tier graph and source kind.";
      type = "app";
      program = "${sandboxImageChecks.imageTierGraphTest}/bin/test-image-tier-graph";
    };

    image-nix-config = {
      meta.description = "Verify baked profile images enable flakes and disable the in-container Nix sandbox.";
      type = "app";
      program = "${sandboxImageChecks.imageNixConfigTest}/bin/test-image-nix-config";
    };

    image-ca-certificates = {
      meta.description = "Verify baked profile images contain CA certificates and SSL_CERT_FILE points at them.";
      type = "app";
      program = "${sandboxImageChecks.imageCaCertificatesTest}/bin/test-image-ca-certificates";
    };

    image-entrypoint-command = {
      meta.description = "Verify the selected platform entrypoint is the image startup command.";
      type = "app";
      program = "${sandboxImageChecks.imageEntrypointCommandTest}/bin/test-image-entrypoint-command";
    };

    image-agent-marker = {
      meta.description = "Verify profile images declare the selected agent in /etc/wrix/image-agent.";
      type = "app";
      program = "${sandboxImageChecks.imageAgentMarkerTest}/bin/test-image-agent-marker";
    };

    profile-packages-bundled = {
      meta.description = "Verify profile package derivations are materialized in emitted image layers.";
      type = "app";
      program = "${sandboxImageChecks.profilePackagesBundledTest}/bin/test-profile-packages-bundled";
    };

    image-tier-membership = {
      meta.description = "Verify non-base profile-image tiers skip lower-tier closures.";
      type = "app";
      program = "${sandboxImageChecks.imageTierMembershipTest}/bin/test-image-tier-membership";
    };

    wrix-images-source-kind = {
      meta.description = "Verify built-in wrix profile images expose platform source kinds.";
      type = "app";
      program = "${sandboxImageChecks.wrixImagesSourceKindTest}/bin/test-wrix-images-source-kind";
    };

    wrix-image-labels = {
      meta.description = "Verify wrix-managed image labels on profile and support images.";
      type = "app";
      program = "${sandboxImageChecks.wrixImageLabelsTest}/bin/test-wrix-image-labels";
    };

    prek-hooks-closure = {
      meta.description = "Verify default sandbox image closure contains the prek hooks bundle";
      type = "app";
      program = "${sandboxImageChecks.prekHooksClosureTest}/bin/test-prek-hooks-closure";
    };

    base-image-universal = {
      meta.description = "Verify wrix-base-image holds only universal bottom-of-closure paths (no profile-specific rustc)";
      type = "app";
      program = "${sandboxImageChecks.baseImageUniversalTest}/bin/test-base-image-universal";
    };

    entrypoint-resolver-base = {
      meta.description = "Verify sandbox images include getent for entrypoint allowlist resolution";
      type = "app";
      program = "${sandboxImageChecks.entrypointResolverBaseTest}/bin/test-entrypoint-resolver-base";
    };

    base-image-hash-stable = {
      meta.description = "Verify wrix-base-image drvPath is invariant under profile-level input changes";
      type = "app";
      program = "${sandboxImageChecks.baseImageHashStableTest}/bin/test-base-image-hash-stable";
    };

    stable-profile-hash-stable = {
      meta.description = "Verify wrix-stable-profile-<name> drvPath is invariant under tier-2 input changes";
      type = "app";
      program = "${sandboxImageChecks.stableProfileHashStableTest}/bin/test-stable-profile-hash-stable";
    };

    stable-profile-membership = {
      meta.description = "Verify wrix-stable-profile-<name> excludes downstream packages and the agent runtime (tier-2 leaf)";
      type = "app";
      program = "${sandboxImageChecks.stableProfileMembershipTest}/bin/test-stable-profile-membership";
    };

    pinned-toolchain-stable-tier = {
      meta.description = "Verify a downstream-pinned rust toolchain lands in tier 1 (wrix-stable-profile-<name>), not the leaf";
      type = "app";
      program = "${sandboxImageChecks.pinnedToolchainStableTest}/bin/test-pinned-toolchain-stable-tier";
    };

    downstream-change-leaf-only = {
      meta.description = "Verify a leaf change leaves every tier-0, tier-1, and tier-2 layer blob byte-identical.";
      type = "app";
      program = "${sandboxImageChecks.downstreamChangeLeafOnlyTest}/bin/test-downstream-change-leaf-only";
    };

    archiveless-generated-change = {
      meta.description = "Verify generated metadata changes only the descriptor and top customisation layer.";
      type = "app";
      program = "${sandboxImageChecks.archivelessGeneratedChangeTest}/bin/test-archiveless-generated-change";
    };

    agent-tier-isolated = {
      meta.description = "Verify the agent runtime rides its own tier while lower-tier blobs stay byte-identical.";
      type = "app";
      program = "${sandboxImageChecks.agentTierIsolatedTest}/bin/test-agent-tier-isolated";
    };

    agent-exclusive = {
      meta.description = "Verify Wrix automatically adds only the selected runtime.";
      type = "app";
      program = "${sandboxImageChecks.agentExclusiveTest}/bin/test-agent-exclusive";
    };

    agent-declared-direct-runner = {
      meta.description = "Verify direct images contain the consumer's declared executable.";
      type = "app";
      program = "${sandboxImageChecks.agentDeclaredDirectRunnerTest}/bin/test-agent-declared-direct-runner";
    };

    agent-consumer-runtime-closures = {
      meta.description = "Verify direct images preserve consumer runtime closures in their normal tiers.";
      type = "app";
      program = "${sandboxImageChecks.agentConsumerRuntimeClosuresTest}/bin/test-agent-consumer-runtime-closures";
    };

    agent-claude-runtime = {
      meta.description = "Verify claude profile images contain claude-code.";
      type = "app";
      program = "${sandboxImageChecks.agentClaudeRuntimeTest}/bin/test-agent-claude-runtime";
    };

    agent-pkg-threaded = {
      meta.description = "Verify agentPkg is owned by the selected agent image tier.";
      type = "app";
      program = "${sandboxImageChecks.agentPkgThreadedTest}/bin/test-agent-pkg-threaded";
    };

    iteration-cost-bounded = {
      meta.description = "Verify a one-wrapper-script perturbation only re-emits the customisation layer + dependent top layers (Linux only)";
      type = "app";
      program = "${sandboxImageChecks.iterationCostBoundedTest}/bin/test-iteration-cost-bounded";
    };

    customisation-layer-bounded = {
      meta.description = "Verify the customisation layer elides Nix's 8 MiB gc-reserved-space padding and stays bounded (Linux only)";
      type = "app";
      program = "${sandboxImageChecks.customisationLayerBoundedTest}/bin/test-customisation-layer-bounded";
    };

    image-nix-db-consistent = {
      meta.description = "Verify the baked image's Nix DB registers its full on-disk store with no orphan.";
      type = "app";
      program = "${sandboxImageChecks.imageNixDbConsistentTest}/bin/test-image-nix-db-consistent";
    };

    image-nix-db-no-dangling = {
      meta.description = "Verify the baked image's Nix DB registers no dangling path.";
      type = "app";
      program = "${sandboxImageChecks.imageNixDbNoDanglingTest}/bin/test-image-nix-db-no-dangling";
    };

    ci = {
      meta.description = "Run full CI-only image and profile verifiers.";
      type = "app";
      program = "${testCi}/bin/test-ci";
    };

    notify = {
      meta.description = "Verify notification client, daemon, and container transport contracts.";
      type = "app";
      program = "${testNotify}/bin/test-notify";
    };

    # profiles.rust.buildPackage [verify] hash invariants (specs/profiles.md).
    profiles-build-package = {
      meta.description = "Verify profiles.rust.buildPackage hash invariants (bin/clippy/nextest/cargoArtifacts)";
      type = "app";
      program = "${testProfilesBuildPackage}/bin/test-profiles-build-package";
    };
  };

  # Individual test sets (for debugging/selective running)
  inherit
    darwinMountTests
    darwinNetworkTests
    darwinUidTests
    readmeTest
    ciAppDerivations
    testAppDerivations
    ciChecks
    linuxBuilderChecks
    rustChecks
    utilityTests
    smokeTests
    systemTests
    testCi
    testImages
    tmuxTests
    verify
    ;
}
