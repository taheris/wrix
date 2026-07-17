# Smoke tests - pure Nix tests that don't require Podman runtime
{
  pkgs,
  system,
  treefmt,
  crane,
  fenix,
  serviceCli ? null,
}:

let
  inherit (pkgs)
    bash
    runCommandLocal
    writeShellApplication
    writeShellScriptBin
    ;
  inherit (builtins) elem getEnv;
  inherit (pkgs.lib) getName;

  isLinux = elem system [
    "x86_64-linux"
    "aarch64-linux"
  ];
  inherit (pkgs.stdenv) isDarwin;

  # Skip heavy image tests when SKIP_IMAGE_TEST=1 (saves ~20s)
  skipImageTest = getEnv "SKIP_IMAGE_TEST" != "";

  # Use Linux packages for image building (requires remote builder on Darwin)
  # Must apply same overlay as flake.nix to get pkgs.beads
  linuxPkgs =
    if isDarwin then
      import pkgs.path {
        system = "aarch64-linux";
        config.allowUnfree = true;
        inherit (pkgs) overlays;
      }
    else
      pkgs;

  profiles = import ../../lib/sandbox/profiles.nix {
    pkgs = linuxPkgs;
    inherit treefmt;
  };

  baseImage = import ../../lib/sandbox/image.nix {
    pkgs = linuxPkgs;
    profile = profiles.base;
    agent = "claude";
    agentPkg = linuxPkgs.claude-code;
    entrypointSh =
      if isDarwin then ../../lib/sandbox/darwin/entrypoint.sh else ../../lib/sandbox/linux/entrypoint.sh;
    claudeConfig = { };
    claudeSettings = { };
    asTarball = isDarwin;
  };

  serviceCliStub =
    if serviceCli != null then
      serviceCli
    else
      writeShellApplication {
        name = "wrix-service-smoke-stub";
        text = ''
          echo "wrix service stub is not executable in smoke tests" >&2
          exit 64
        '';
      };

  sandboxLib = import ../../lib/sandbox {
    inherit
      pkgs
      system
      linuxPkgs
      treefmt
      crane
      fenix
      ;
    serviceCli = serviceCliStub;
  };
  sandbox = sandboxLib.mkSandbox { profile = sandboxLib.profiles.base; };
  wrix = sandbox.package;
  wrixBuilder = import ../../lib/builder { inherit pkgs linuxPkgs; };
  wrixLauncher = sandbox.launcher;

  pathProbeCli = writeShellScriptBin "wrix" ''
    set -euo pipefail
    printf 'NIX_STORE=%s\n' "$(command -v nix-store)"
    ${
      if isLinux then
        ''
          printf 'PODMAN=%s\n' "$(command -v podman)"
          printf 'SKOPEO=%s\n' "$(command -v skopeo)"
        ''
      else if isDarwin then
        ''
          printf 'SKOPEO=%s\n' "$(command -v skopeo)"
        ''
      else
        ""
    }
  '';
  pathProbeSandboxLib = import ../../lib/sandbox {
    inherit
      pkgs
      system
      linuxPkgs
      treefmt
      crane
      fenix
      ;
    serviceCli = pathProbeCli;
  };
  pathProbePackage =
    (pathProbeSandboxLib.mkSandbox { profile = pathProbeSandboxLib.profiles.base; }).package;

in
{
  # Verify OCI image builds and is a valid tar archive.
  # Linux-only for `nix flake check`: Darwin requires a remote Linux builder,
  # so host-side image build coverage lives in the explicit image packages/apps.
  # Skip with SKIP_IMAGE_TEST=1 for faster iteration (saves ~20s).
  image-builds =
    if skipImageTest || !isLinux then
      runCommandLocal "smoke-image-builds-skipped" { } ''
        trap '_ec=$?; if [ "$_ec" -eq 77 ]; then mkdir -p $out; exit 0; fi' EXIT
        echo "SKIP: Image build test (requires Linux host or explicit image build)" >&2
        exit 77
      ''
    else
      runCommandLocal "smoke-image-builds" { } ''
        echo "Checking base image..."
        test -x ${baseImage}
        ${baseImage} | tar -tf - >/dev/null

        echo "Image built successfully"
        mkdir $out
      '';

  script-syntax =
    runCommandLocal "smoke-rust-launcher-dry-run"
      {
        nativeBuildInputs = [ bash ];
      }
      ''
        workspace="$PWD/workspace"
        home="$PWD/home"
        profile_config="$PWD/profile-config.json"
        mkdir -p "$workspace" "$home"
        cat > "$profile_config" <<'JSON'
        {"schema":1,"system":"test","profile":{"name":"base","env":{},"mounts":[],"writable_dirs":[],"network_allowlist":[]},"image":{"ref":"wrix-base:test","source":"/nix/store/fake-image","source_kind":"${
          if isLinux then "nix-descriptor" else "docker-archive"
        }","digest":"sha256:test"},"agent":{"kind":"direct"},"resources":{"cpus":null,"memory_mb":4096,"pids_limit":4096},"security":{"deploy_key":null},"network":{"default_mode":"open","ipv6":"disabled"},"services":{"beads":{"enable":"auto"},"nix_cache":{"enable":false}},"features":{"mcp_runtime":false}}
        JSON

        output=$(HOME="$home" WRIX_DRY_RUN=1 ${wrixLauncher}/bin/wrix --profile-config "$profile_config" run "$workspace" true)
        case "$output" in
          *"SUBCOMMAND=run"*"PROFILE_AGENT=direct"*"WORKSPACE=$workspace"*"CMD=true"*) ;;
          *)
            printf 'launcher dry-run did not expose expected Rust launcher state:\n%s\n' "$output" >&2
            exit 1
            ;;
        esac

        mkdir $out
      '';

  package-script-syntax =
    runCommandLocal "smoke-package-script-syntax"
      {
        nativeBuildInputs = [ bash ];
      }
      ''
        echo "Checking sandbox package wrapper syntax..."
        ${
          if isLinux then
            "bash -n ${wrix}/bin/wrix"
          else
            "echo \"SKIP: sandbox package wrapper syntax (Linux-only)\" >&2"
        }
        mkdir $out
      '';

  package-runtime-path =
    runCommandLocal "smoke-package-runtime-path" { nativeBuildInputs = [ bash ]; }
      ''
        set -euo pipefail

        check_output() {
          local label="$1"
          local output="$2"
          case "$output" in
            *"NIX_STORE=/nix/store/"*) ;;
            *)
              printf '%s wrapper did not make nix-store available on PATH:\n%s\n' "$label" "$output" >&2
              exit 1
              ;;
          esac
          ${
            if isLinux then
              ''
                case "$output" in
                  *"PODMAN=/nix/store/"*) ;;
                  *)
                    printf '%s wrapper did not make podman available on PATH:\n%s\n' "$label" "$output" >&2
                    exit 1
                    ;;
                esac
                case "$output" in
                  *"SKOPEO=/nix/store/"*) ;;
                  *)
                    printf '%s wrapper did not make skopeo available on PATH:\n%s\n' "$label" "$output" >&2
                    exit 1
                    ;;
                esac
              ''
            else if isDarwin then
              ''
                case "$output" in
                  *"SKOPEO=/nix/store/"*) ;;
                  *)
                    printf '%s wrapper did not make skopeo available on PATH:\n%s\n' "$label" "$output" >&2
                    exit 1
                    ;;
                esac
              ''
            else
              ""
          }
        }

        mkdir -p home
        explicit=$(env -i HOME="$PWD/home" PATH=/path-not-used ${pathProbePackage}/bin/wrix run)
        default=$(env -i HOME="$PWD/home" PATH=/path-not-used ${pathProbePackage}/bin/wrix-run)
        check_output "bin/wrix" "$explicit"
        check_output "bin/wrix-run" "$default"

        mkdir $out
      '';

  # Verify Darwin entrypoint script syntax and mount handling logic
  darwin-entrypoint-syntax =
    runCommandLocal "smoke-darwin-entrypoint"
      {
        nativeBuildInputs = [
          bash
          pkgs.coreutils
        ];
      }
      ''
        echo "Checking Darwin entrypoint syntax..."
        bash -n ${../../lib/sandbox/darwin/entrypoint.sh}

        echo "Verifying entrypoint handles mount env vars..."
        # Test that entrypoint processes WRIX_DIR_MOUNTS correctly
        SCRIPT="${../../lib/sandbox/darwin/entrypoint.sh}"
        grep -q 'WRIX_DIR_MOUNTS' "$SCRIPT" || { echo "Missing WRIX_DIR_MOUNTS handling"; exit 1; }
        grep -q 'WRIX_FILE_MOUNTS' "$SCRIPT" || { echo "Missing WRIX_FILE_MOUNTS handling"; exit 1; }

        # Verify entrypoint uses fixed wrix user
        grep -q 'USER="wrix"' "$SCRIPT" || { echo "entrypoint should set USER=wrix"; exit 1; }
        grep -q 'HOME="/home/wrix"' "$SCRIPT" || { echo "entrypoint should set HOME=/home/wrix"; exit 1; }
        grep -q 'WRIX_AGENT" = "pi".*WRIX_STDIO' "$SCRIPT" || { echo "Pi RPC mode must be gated by WRIX_STDIO"; exit 1; }
        grep -q 'pi || MAIN_EXIT' "$SCRIPT" || { echo "Pi interactive mode must run plain pi"; exit 1; }
        grep -q '.pi/agent/sessions' "$SCRIPT" || { echo "Pi sessions directory must be initialized"; exit 1; }
        grep -q 'WRIX_PI_AUTH_JSON' "$SCRIPT" || { echo "Pi auth mount must be linked into agent config"; exit 1; }
        ! grep -q '/workspace/.pi/agent/\*' "$SCRIPT" || { echo "Pi must not import arbitrary workspace agent config"; exit 1; }
        grep -q 'WRIX_STDIO=1' ${../../lib/sandbox/darwin/default.nix} || { echo "Darwin launcher must forward WRIX_STDIO"; exit 1; }

        echo "Darwin entrypoint validation passed"
        mkdir $out
      '';

  # Verify Linux entrypoint script syntax
  linux-entrypoint-syntax =
    runCommandLocal "smoke-linux-entrypoint"
      {
        nativeBuildInputs = [ bash ];
      }
      ''
        echo "Checking Linux entrypoint syntax..."
        bash -n ${../../lib/sandbox/linux/entrypoint.sh}
        SCRIPT="${../../lib/sandbox/linux/entrypoint.sh}"
        grep -q 'WRIX_AGENT" = "pi".*WRIX_STDIO' "$SCRIPT" || { echo "Pi RPC mode must be gated by WRIX_STDIO"; exit 1; }
        grep -q 'pi || MAIN_EXIT' "$SCRIPT" || { echo "Pi interactive mode must run plain pi"; exit 1; }
        grep -q '.pi/agent/sessions' "$SCRIPT" || { echo "Pi sessions directory must be initialized"; exit 1; }
        grep -q 'WRIX_PI_AUTH_JSON' "$SCRIPT" || { echo "Pi auth mount must be linked into agent config"; exit 1; }
        ! grep -q '/workspace/.pi/agent/\*' "$SCRIPT" || { echo "Pi must not import arbitrary workspace agent config"; exit 1; }

        echo "Linux entrypoint validation passed"
        mkdir $out
      '';

  # Verify Pi defaults and image config are scoped to the Pi agent tier.
  pi-config-defaults = runCommandLocal "smoke-pi-config-defaults" { } ''
    DEFAULTS="${../../lib/sandbox/default.nix}"
    IMAGE="${../../lib/sandbox/image.nix}"

    grep -q 'defaultProvider = "openai-codex"' "$DEFAULTS" || { echo "Pi default provider must be OpenAI Codex"; exit 1; }
    grep -q 'defaultModel = "gpt-5.6-sol"' "$DEFAULTS" || { echo "Pi default model must be gpt-5.6-sol"; exit 1; }
    grep -q 'defaultThinkingLevel = "xhigh"' "$DEFAULTS" || { echo "Pi default reasoning must be xhigh"; exit 1; }
    grep -q 'defaultProjectTrust = "always"' "$DEFAULTS" || { echo "Pi must trust /workspace by default"; exit 1; }
    grep -q 'steeringMode = "all"' "$DEFAULTS" || { echo "Pi steering mode must default to all"; exit 1; }
    grep -q 'followUpMode = "all"' "$DEFAULTS" || { echo "Pi follow-up mode must default to all"; exit 1; }
    grep -q 'sessionDir = "/workspace/.pi/agent/sessions"' "$DEFAULTS" || { echo "Pi sessions must persist via explicit sessionDir"; exit 1; }
    grep -q 'optionalString (agent == "pi")' "$IMAGE" || { echo "Pi settings must only be baked for agent=pi"; exit 1; }
    grep -q 'etc/wrix/pi-agent/settings.json' "$IMAGE" || { echo "Pi settings seed must be written under /etc/wrix/pi-agent"; exit 1; }

    echo "Pi config defaults validation passed"
    mkdir $out
  '';

  # Verify builder key generation produces host-user-owned material.
  builder-keys-structure =
    runCommandLocal "smoke-builder-keys"
      {
        nativeBuildInputs = [
          bash
          pkgs.coreutils
          pkgs.gnugrep
          pkgs.gnutar
          pkgs.openssh
        ];
      }
      ''
        echo "Checking builder key structure..."
        WRIX_BUILDER_BIN="${wrixBuilder}/bin/wrix-builder" REPO_ROOT="${../..}" bash "${../../tests/builder/key-material.sh}"
        mkdir $out
      '';

  # Verify builder sshd security configuration
  # Security properties:
  # - PasswordAuthentication no (key-based auth only)
  # - PermitRootLogin no (no root access)
  # - AllowUsers builder (single non-root user)
  builder-sshd-security =
    runCommandLocal "smoke-builder-sshd-security"
      {
        nativeBuildInputs = [ bash ];
      }
      ''
        echo "Checking builder sshd security configuration..."
        SCRIPT="${../../lib/sandbox/builder/entrypoint.sh}"

        # Verify password authentication is disabled
        grep -q 'PasswordAuthentication no' "$SCRIPT" || { echo "FAIL: PasswordAuthentication must be 'no'"; exit 1; }
        echo "PASS: Password authentication disabled"

        # Verify root login is disabled
        grep -q 'PermitRootLogin no' "$SCRIPT" || { echo "FAIL: PermitRootLogin must be 'no'"; exit 1; }
        echo "PASS: Root login disabled"

        # Verify only builder user is allowed (uses $BUILDER_USER variable which is set to "builder")
        grep -q 'AllowUsers \$BUILDER_USER' "$SCRIPT" || { echo "FAIL: AllowUsers must restrict to BUILDER_USER"; exit 1; }
        grep -q 'BUILDER_USER="builder"' "$SCRIPT" || { echo "FAIL: BUILDER_USER must be set to builder"; exit 1; }
        echo "PASS: SSH access restricted to builder user"

        # Verify no password hash in /etc/passwd (login disabled)
        # The entrypoint creates user with 'x' password field (no password login)
        grep -q '\$BUILDER_USER:x:' "$SCRIPT" || { echo "FAIL: Builder user should have 'x' password field"; exit 1; }
        echo "PASS: Builder user has no password set"

        echo ""
        echo "Builder sshd security validation passed"
        mkdir $out
      '';

  # Verify builder SSH port is bound to localhost only
  # Security property: prevents remote access to builder SSH service
  # Binding to 127.0.0.1 instead of 0.0.0.0 ensures only local connections
  builder-ssh-localhost-only =
    runCommandLocal "smoke-builder-ssh-localhost"
      {
        nativeBuildInputs = [ bash ];
      }
      ''
        echo "Checking builder SSH port is localhost-only..."
        SCRIPT="${../../lib/builder/default.nix}"

        # Verify SSH port is bound to 127.0.0.1 (localhost), not 0.0.0.0 or unbound
        # The pattern should be: -p "127.0.0.1:$SSH_PORT:22"
        grep -q '127\.0\.0\.1:\$SSH_PORT:22' "$SCRIPT" || { echo "FAIL: SSH port must bind to 127.0.0.1 (localhost only)"; exit 1; }
        echo "PASS: SSH port bound to localhost only"

        # Verify there's no 0.0.0.0 binding (which would allow remote access)
        if grep -q '0\.0\.0\.0:\$SSH_PORT' "$SCRIPT"; then
          echo "FAIL: SSH port must NOT bind to 0.0.0.0 (allows remote access)"
          exit 1
        fi
        echo "PASS: No 0.0.0.0 binding found"

        echo ""
        echo "Builder SSH localhost binding validation passed"
        mkdir $out
      '';

  # Verify mkSandbox accepts mcp parameter and configures servers correctly
  # Pure eval test: mkSandbox evaluates without error for all MCP configurations
  # No image build needed — we only check that the Nix expressions evaluate
  mcp-sandbox-configuration =
    let
      # These force Nix evaluation of mkSandbox with various MCP configs
      # If any fail to evaluate, the derivation won't be created
      sandboxWithMcp = sandboxLib.mkSandbox {
        profile = sandboxLib.profiles.base;
        mcp = {
          tmux = { };
        };
      };

      sandboxWithMcpAudit = sandboxLib.mkSandbox {
        profile = sandboxLib.profiles.base;
        mcp = {
          tmux = {
            audit = "/workspace/.debug-audit.log";
          };
        };
      };

      sandboxNoMcp = sandboxLib.mkSandbox {
        profile = sandboxLib.profiles.base;
      };

      # Force evaluation of profile attrsets (cheap, no image build)
      checks = builtins.seq sandboxWithMcp.profile (
        builtins.seq sandboxWithMcpAudit.profile (builtins.seq sandboxNoMcp.profile true)
      );
    in
    assert checks;
    runCommandLocal "smoke-mcp-sandbox-configuration" { } ''
      echo "PASS: mkSandbox evaluates with all MCP configurations"
      mkdir $out
    '';

  # Regression guard for wx-1thzk.1: treefmt must be in basePackages so
  # all sandbox containers inherit the project formatter wrapper.
  profiles-contain-treefmt =
    let
      hasTreefmt = profile: elem treefmt profile.packages;
      hasTreefmtByName =
        profile: builtins.any (p: (p.pname or p.name or "") == "treefmt") profile.packages;
      checks =
        assert hasTreefmt sandboxLib.profiles.base;
        assert hasTreefmtByName sandboxLib.profiles.base;
        assert hasTreefmt sandboxLib.profiles.rust;
        assert hasTreefmt sandboxLib.profiles.python;
        true;
    in
    assert checks;
    runCommandLocal "smoke-profiles-contain-treefmt" { } ''
      echo "PASS: treefmt present in base, rust, and python profile packages"
      mkdir $out
    '';

  linux-microvm-krun-detection =
    if isLinux then
      runCommandLocal "smoke-linux-microvm-krun"
        {
          nativeBuildInputs = [
            bash
            pkgs.gnugrep
          ];
        }
        ''
          launcher="${wrixLauncher}/bin/wrix"
          grep -aq 'WRIX_MICROVM' "$launcher" || { echo "FAIL: Missing WRIX_MICROVM env var check"; exit 1; }
          grep -aq '/dev/kvm' "$launcher" || { echo "FAIL: Missing /dev/kvm detection"; exit 1; }
          grep -aq -- '--runtime' "$launcher" || { echo "FAIL: Missing runtime flag construction"; exit 1; }
          echo "Linux microVM krun detection validation passed"
          mkdir $out
        ''
    else
      runCommandLocal "smoke-linux-microvm-krun" { } ''
        trap '_ec=$?; if [ "$_ec" -eq 77 ]; then mkdir -p $out; exit 0; fi' EXIT
        echo "SKIP: krun microVM detection (Linux-only test)" >&2
        exit 77
      '';

  linux-pasta-port-forwarding-disabled =
    if isLinux then
      runCommandLocal "smoke-linux-pasta-port-forwarding-disabled"
        {
          nativeBuildInputs = [ bash ];
        }
        ''
          set -euo pipefail

          workspace="$PWD/workspace"
          home="$PWD/home"
          profile_config="$PWD/profile-config.json"
          mkdir -p "$workspace" "$home"
          cat > "$profile_config" <<'JSON'
          {"schema":1,"system":"test","profile":{"name":"base","env":{},"mounts":[],"writable_dirs":[],"network_allowlist":[]},"image":{"ref":"wrix-base:test","source":"/nix/store/fake-image","source_kind":"nix-descriptor","digest":"sha256:test"},"agent":{"kind":"direct"},"resources":{"cpus":null,"memory_mb":4096,"pids_limit":4096},"security":{"deploy_key":null},"network":{"default_mode":"open","ipv6":"disabled"},"services":{"beads":{"enable":"auto"},"nix_cache":{"enable":false}},"features":{"mcp_runtime":false}}
          JSON

          output=$(HOME="$home" WRIX_DRY_RUN=1 ${wrixLauncher}/bin/wrix --profile-config "$profile_config" run "$workspace" true)
          case "$output" in
            *"PODMAN_NETWORK=pasta:"*"--map-host-loopback,169.254.1.2"*"--map-guest-addr,none"*"-t,none"*"-u,none"*"-T,none"*"-U,none"*) ;;
            *)
              printf 'launcher dry-run did not disable pasta auto-forwarding:\n%s\n' "$output" >&2
              exit 1
              ;;
          esac
          case "$output" in
            *"-t,auto"*|*"-u,auto"*|*"-T,auto"*|*"-U,auto"*)
              printf 'launcher dry-run enabled pasta auto-forwarding:\n%s\n' "$output" >&2
              exit 1
              ;;
          esac

          mkdir $out
        ''
    else
      runCommandLocal "smoke-linux-pasta-port-forwarding-disabled" { } ''
        trap '_ec=$?; if [ "$_ec" -eq 77 ]; then mkdir -p $out; exit 0; fi' EXIT
        echo "SKIP: pasta forwarding guard (Linux-only test)" >&2
        exit 77
      '';

  # Verify WRIX_NETWORK environment variable support in launcher and entrypoints
  # Security property: network filtering restricts outbound access in limit mode
  # See specs/security.md § Network Exfil Baseline and specs/sandbox.md.
  #
  # Profile allowlists are verified as pure Nix assertions (no image build needed).
  # Launcher script checks reuse the base sandbox (already built by image-builds).
  network-mode-configuration =
    let
      inherit (builtins) elem;
      baseAllowlist = sandboxLib.profiles.base.networkAllowlist;
      rustAllowlist = sandboxLib.profiles.rust.networkAllowlist;
      pythonAllowlist = sandboxLib.profiles.python.networkAllowlist;
      basePackageNames = map getName sandboxLib.profiles.base.packages;

      # Pure Nix assertions for profile allowlists and firewall tools (no image build)
      allowlistChecks =
        assert elem "nftables" basePackageNames;
        assert elem "iptables" basePackageNames;
        assert elem "api.anthropic.com" baseAllowlist;
        assert elem "github.com" baseAllowlist;
        assert elem "cache.nixos.org" baseAllowlist;
        assert elem "crates.io" rustAllowlist;
        assert elem "static.crates.io" rustAllowlist;
        assert elem "index.crates.io" rustAllowlist;
        assert elem "pypi.org" pythonAllowlist;
        assert elem "files.pythonhosted.org" pythonAllowlist;
        true;
    in
    assert allowlistChecks;
    runCommandLocal "smoke-network-mode"
      {
        nativeBuildInputs = [
          bash
          pkgs.gnugrep
        ];
      }
      ''
        echo "Checking WRIX_NETWORK support..."

        echo "PASS: Profile allowlists verified (pure Nix assertions)"

        workspace="$PWD/workspace"
        home="$PWD/home"
        profile_config="$PWD/profile-config.json"
        mkdir -p "$workspace" "$home"
        cat > "$profile_config" <<'JSON'
        {"schema":1,"system":"test","profile":{"name":"base","env":{},"mounts":[],"writable_dirs":[],"network_allowlist":["api.anthropic.com"]},"image":{"ref":"wrix-base:test","source":"/nix/store/fake-image","source_kind":"${
          if isLinux then "nix-descriptor" else "docker-archive"
        }","digest":"sha256:test"},"agent":{"kind":"direct"},"resources":{"cpus":null,"memory_mb":4096,"pids_limit":4096},"security":{"deploy_key":null},"network":{"default_mode":"open","ipv6":"disabled"},"services":{"beads":{"enable":"auto"},"nix_cache":{"enable":false}},"features":{"mcp_runtime":false}}
        JSON
        output=$(HOME="$home" WRIX_DRY_RUN=1 WRIX_NETWORK=limit ${wrixLauncher}/bin/wrix --profile-config "$profile_config" run "$workspace" true)
        case "$output" in
          *"PROFILE_AGENT=direct"*) ;;
          *) printf 'launcher dry-run did not accept WRIX_NETWORK=limit:\n%s\n' "$output" >&2; exit 1 ;;
        esac
        if HOME="$home" WRIX_DRY_RUN=1 WRIX_NETWORK=unsafe ${wrixLauncher}/bin/wrix --profile-config "$profile_config" run "$workspace" true >/tmp/wrix-network.out 2>/tmp/wrix-network.err; then
          echo "FAIL: launcher accepted invalid WRIX_NETWORK" >&2
          exit 1
        fi
        grep -q "WRIX_NETWORK must be 'open' or 'limit'" /tmp/wrix-network.err || { cat /tmp/wrix-network.err >&2; exit 1; }

        # Entrypoint checks (source files, no build needed)
        LINUX_EP="${../../lib/sandbox/linux/entrypoint.sh}"
        grep -q 'WRIX_FIREWALL_BACKEND="nft"' "$LINUX_EP" || { echo "FAIL: Missing nft firewall default in Linux entrypoint"; exit 1; }
        grep -q 'WRIX_FIREWALL_BACKEND="iptables"' "$LINUX_EP" || { echo "FAIL: Missing iptables fallback in Linux entrypoint"; exit 1; }
        grep -q 'WRIX_NETWORK' "$LINUX_EP" || { echo "FAIL: Missing WRIX_NETWORK check in Linux entrypoint"; exit 1; }
        echo "PASS: Linux entrypoint has network filtering"

        DARWIN_EP="${../../lib/sandbox/darwin/entrypoint.sh}"
        grep -q 'WRIX_FIREWALL_BACKEND="nft"' "$DARWIN_EP" || { echo "FAIL: Missing nft firewall default in Darwin entrypoint"; exit 1; }
        grep -q 'WRIX_FIREWALL_BACKEND="iptables"' "$DARWIN_EP" || { echo "FAIL: Missing iptables fallback in Darwin entrypoint"; exit 1; }
        grep -q 'WRIX_NETWORK' "$DARWIN_EP" || { echo "FAIL: Missing WRIX_NETWORK check in Darwin entrypoint"; exit 1; }
        echo "PASS: Darwin entrypoint has network filtering"

        echo ""
        echo "WRIX_NETWORK configuration validation passed"
        mkdir $out
      '';

  # Verify GitHub SSH host keys match official fingerprints
  # Security property: ensures hardcoded keys haven't drifted from GitHub's published keys
  # Reference: https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/githubs-ssh-key-fingerprints
  github-known-hosts-fingerprints =
    let
      knownHosts = import ../../lib/sandbox/known-hosts.nix { inherit pkgs; };
    in
    runCommandLocal "smoke-github-fingerprints"
      {
        nativeBuildInputs = [
          pkgs.openssh
          pkgs.gnugrep
        ];
      }
      ''
        echo "Verifying GitHub SSH host keys against official fingerprints..."

        # Official SHA256 fingerprints from GitHub documentation:
        # https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/githubs-ssh-key-fingerprints
        EXPECTED_ED25519="SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU"
        EXPECTED_RSA="SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s"
        EXPECTED_ECDSA="SHA256:p2QAMXNIC1TJYWeIOttrVc98/R1BUFWu3/LiyKgUfQM"

        KNOWN_HOSTS="${knownHosts}/known_hosts"

        # Verify Ed25519 key fingerprint
        ED25519_FP=$(ssh-keygen -lf <(grep "ssh-ed25519" "$KNOWN_HOSTS") | awk '{print $2}')
        if [ "$ED25519_FP" != "$EXPECTED_ED25519" ]; then
          echo "FAIL: Ed25519 fingerprint mismatch"
          echo "  Expected: $EXPECTED_ED25519"
          echo "  Got:      $ED25519_FP"
          exit 1
        fi
        echo "PASS: Ed25519 fingerprint matches ($EXPECTED_ED25519)"

        # Verify RSA key fingerprint
        RSA_FP=$(ssh-keygen -lf <(grep "ssh-rsa" "$KNOWN_HOSTS") | awk '{print $2}')
        if [ "$RSA_FP" != "$EXPECTED_RSA" ]; then
          echo "FAIL: RSA fingerprint mismatch"
          echo "  Expected: $EXPECTED_RSA"
          echo "  Got:      $RSA_FP"
          exit 1
        fi
        echo "PASS: RSA fingerprint matches ($EXPECTED_RSA)"

        # Verify ECDSA key fingerprint
        ECDSA_FP=$(ssh-keygen -lf <(grep "ecdsa-sha2" "$KNOWN_HOSTS") | awk '{print $2}')
        if [ "$ECDSA_FP" != "$EXPECTED_ECDSA" ]; then
          echo "FAIL: ECDSA fingerprint mismatch"
          echo "  Expected: $EXPECTED_ECDSA"
          echo "  Got:      $ECDSA_FP"
          exit 1
        fi
        echo "PASS: ECDSA fingerprint matches ($EXPECTED_ECDSA)"

        echo ""
        echo "All GitHub SSH host keys verified against official fingerprints"
        echo "Reference: https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/githubs-ssh-key-fingerprints"
        mkdir $out
      '';
}
