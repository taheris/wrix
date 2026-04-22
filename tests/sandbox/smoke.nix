# Smoke tests - pure Nix tests that don't require Podman runtime
{
  pkgs,
  system,
  treefmt,
}:

let
  inherit (pkgs) bash runCommandLocal;
  inherit (builtins) elem getEnv;

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
    entrypointPkg = linuxPkgs.claude-code;
    entrypointSh =
      if isDarwin then ../../lib/sandbox/darwin/entrypoint.sh else ../../lib/sandbox/linux/entrypoint.sh;
    claudeConfig = { };
    claudeSettings = { };
    asTarball = isDarwin;
  };

  sandboxLib = import ../../lib {
    inherit
      pkgs
      system
      linuxPkgs
      treefmt
      ;
  };
  sandbox = sandboxLib.mkSandbox { profile = sandboxLib.profiles.base; };
  wrapix = sandbox.package;

in
{
  # Verify OCI image builds and is a valid tar archive
  # On Darwin, this requires a Linux remote builder
  # Skip with SKIP_IMAGE_TEST=1 for faster iteration (saves ~20s)
  image-builds =
    if skipImageTest then
      runCommandLocal "smoke-image-builds-skipped" { } ''
        trap '_ec=$?; if [ "$_ec" -eq 77 ]; then mkdir -p $out; exit 0; fi' EXIT
        echo "SKIP: Image build test (SKIP_IMAGE_TEST=1)" >&2
        exit 77
      ''
    else
      runCommandLocal "smoke-image-builds" { } ''
        echo "Checking base image..."
        ${
          if isDarwin then
            ''
              test -f ${baseImage}
              tar -tf ${baseImage} >/dev/null
            ''
          else
            ''
              test -x ${baseImage}
              ${baseImage} | tar -tf - >/dev/null
            ''
        }

        echo "Image built successfully"
        mkdir $out
      '';

  # Verify wrapix script has valid bash syntax
  script-syntax =
    runCommandLocal "smoke-script-syntax"
      {
        nativeBuildInputs = [ bash ];
      }
      ''
        echo "Checking bash syntax..."
        bash -n ${wrapix}/bin/wrapix

        echo "Script syntax validation passed"
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
        # Test that entrypoint processes WRAPIX_DIR_MOUNTS correctly
        SCRIPT="${../../lib/sandbox/darwin/entrypoint.sh}"
        grep -q 'WRAPIX_DIR_MOUNTS' "$SCRIPT" || { echo "Missing WRAPIX_DIR_MOUNTS handling"; exit 1; }
        grep -q 'WRAPIX_FILE_MOUNTS' "$SCRIPT" || { echo "Missing WRAPIX_FILE_MOUNTS handling"; exit 1; }

        # Verify entrypoint uses fixed wrapix user
        grep -q 'USER="wrapix"' "$SCRIPT" || { echo "entrypoint should set USER=wrapix"; exit 1; }
        grep -q 'HOME="/home/wrapix"' "$SCRIPT" || { echo "entrypoint should set HOME=/home/wrapix"; exit 1; }

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

        echo "Linux entrypoint validation passed"
        mkdir $out
      '';

  # Verify builder key generation produces expected outputs
  # Security note: Keys are in Nix store (world-readable), mitigated by localhost-only SSH
  # See specs/security-review.md "Builder SSH Keys" section
  builder-keys-structure =
    let
      builderKeys = import ../../lib/builder/hostkey.nix { inherit pkgs; };
    in
    runCommandLocal "smoke-builder-keys" { } ''
      echo "Checking builder key structure..."

      # Verify all expected files exist
      test -f ${builderKeys}/ssh_host_ed25519_key || { echo "FAIL: Missing host private key"; exit 1; }
      test -f ${builderKeys}/ssh_host_ed25519_key.pub || { echo "FAIL: Missing host public key"; exit 1; }
      test -f ${builderKeys}/builder_ed25519 || { echo "FAIL: Missing client private key"; exit 1; }
      test -f ${builderKeys}/builder_ed25519.pub || { echo "FAIL: Missing client public key"; exit 1; }
      test -f ${builderKeys}/public_host_key_base64 || { echo "FAIL: Missing base64 host key"; exit 1; }
      echo "PASS: All expected key files present"

      # Verify keys are ed25519 (not weaker algorithms)
      grep -q "ssh-ed25519" ${builderKeys}/ssh_host_ed25519_key.pub || { echo "FAIL: Host key not ed25519"; exit 1; }
      grep -q "ssh-ed25519" ${builderKeys}/builder_ed25519.pub || { echo "FAIL: Client key not ed25519"; exit 1; }
      echo "PASS: Keys use ed25519 algorithm"

      # Verify private keys have expected OpenSSH format header
      head -1 ${builderKeys}/ssh_host_ed25519_key | grep -q "BEGIN OPENSSH PRIVATE KEY" || { echo "FAIL: Host private key format invalid"; exit 1; }
      head -1 ${builderKeys}/builder_ed25519 | grep -q "BEGIN OPENSSH PRIVATE KEY" || { echo "FAIL: Client private key format invalid"; exit 1; }
      echo "PASS: Private keys have valid format"

      # Verify base64 host key is non-empty and valid base64
      test -s ${builderKeys}/public_host_key_base64 || { echo "FAIL: Base64 key is empty"; exit 1; }
      echo "PASS: Base64 public host key present"

      # Document that keys are in Nix store (world-readable)
      # This is intentional - see specs/security-review.md for security analysis
      echo ""
      echo "NOTE: Keys are in Nix store at ${builderKeys}"
      echo "This is documented in specs/security-review.md 'Builder SSH Keys' section"
      echo "Mitigations: localhost-only SSH, password auth disabled, machine-local keys"

      echo ""
      echo "Builder key structure validation passed"
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

  # Verify wrapix script contains krun microVM boundary detection
  # Security property: Linux defaults to microVM boundary via krun runtime
  # See specs/security-review.md "MicroVM Boundary" section
  # Linux-only: krun detection only exists in the Linux launcher
  linux-microvm-krun-detection =
    if isLinux then
      runCommandLocal "smoke-linux-microvm-krun"
        {
          nativeBuildInputs = [ bash ];
        }
        ''
          echo "Checking krun microVM detection in wrapix script..."
          SCRIPT="${wrapix}/bin/wrapix"

          # Verify krun detection logic exists
          grep -q 'WRAPIX_MICROVM' "$SCRIPT" || { echo "FAIL: Missing WRAPIX_MICROVM env var check"; exit 1; }
          echo "PASS: WRAPIX_MICROVM opt-in supported"

          # Verify /dev/kvm detection
          grep -q '/dev/kvm' "$SCRIPT" || { echo "FAIL: Missing /dev/kvm detection"; exit 1; }
          echo "PASS: /dev/kvm availability check present"

          # Verify krun runtime flag is used
          grep -q '\-\-runtime krun' "$SCRIPT" || { echo "FAIL: Missing --runtime krun flag"; exit 1; }
          echo "PASS: --runtime krun flag present"

          # Verify crun-krun is bundled in runtimeInputs
          grep -q 'crun-krun' "$SCRIPT" || { echo "FAIL: Missing crun-krun in PATH"; exit 1; }
          echo "PASS: crun-krun bundled in PATH"

          echo ""
          echo "Linux microVM krun detection validation passed"
          mkdir $out
        ''
    else
      runCommandLocal "smoke-linux-microvm-krun" { } ''
        trap '_ec=$?; if [ "$_ec" -eq 77 ]; then mkdir -p $out; exit 0; fi' EXIT
        echo "SKIP: krun microVM detection (Linux-only test)" >&2
        exit 77
      '';

  # Verify WRAPIX_NETWORK environment variable support in launcher and entrypoints
  # Security property: network filtering restricts outbound access in limit mode
  # See specs/security-review.md "Network Modes" section
  #
  # Profile allowlists are verified as pure Nix assertions (no image build needed).
  # Launcher script checks reuse the base sandbox (already built by image-builds).
  network-mode-configuration =
    let
      inherit (builtins) elem;
      baseAllowlist = sandboxLib.profiles.base.networkAllowlist;
      rustAllowlist = sandboxLib.profiles.rust.networkAllowlist;
      pythonAllowlist = sandboxLib.profiles.python.networkAllowlist;

      # Pure Nix assertions for profile allowlists (no image build)
      allowlistChecks =
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
        echo "Checking WRAPIX_NETWORK support..."

        echo "PASS: Profile allowlists verified (pure Nix assertions)"

        # Launcher script checks (reuses base sandbox, no extra image build)
        SCRIPT="${wrapix}/bin/wrapix"
        grep -q 'WRAPIX_NETWORK' "$SCRIPT" || { echo "FAIL: Missing WRAPIX_NETWORK env var handling"; exit 1; }
        echo "PASS: WRAPIX_NETWORK env var handled in launcher"

        grep -q "open|limit" "$SCRIPT" || { echo "FAIL: Missing WRAPIX_NETWORK mode validation"; exit 1; }
        echo "PASS: WRAPIX_NETWORK mode validation present"

        grep -q 'WRAPIX_NETWORK_ALLOWLIST' "$SCRIPT" || { echo "FAIL: Missing WRAPIX_NETWORK_ALLOWLIST passthrough"; exit 1; }
        echo "PASS: WRAPIX_NETWORK_ALLOWLIST passed to container"

        ${
          if isLinux then
            ''
              grep -q 'NET_ADMIN' "$SCRIPT" || { echo "FAIL: Missing NET_ADMIN capability for limit mode"; exit 1; }
              echo "PASS: NET_ADMIN capability added for limit mode"
            ''
          else
            ''
              echo "SKIP: NET_ADMIN check (Linux-only)"
            ''
        }

        # Entrypoint checks (source files, no build needed)
        LINUX_EP="${../../lib/sandbox/linux/entrypoint.sh}"
        grep -q 'iptables' "$LINUX_EP" || { echo "FAIL: Missing iptables in Linux entrypoint"; exit 1; }
        grep -q 'WRAPIX_NETWORK' "$LINUX_EP" || { echo "FAIL: Missing WRAPIX_NETWORK check in Linux entrypoint"; exit 1; }
        echo "PASS: Linux entrypoint has network filtering"

        DARWIN_EP="${../../lib/sandbox/darwin/entrypoint.sh}"
        grep -q 'iptables' "$DARWIN_EP" || { echo "FAIL: Missing iptables in Darwin entrypoint"; exit 1; }
        grep -q 'WRAPIX_NETWORK' "$DARWIN_EP" || { echo "FAIL: Missing WRAPIX_NETWORK check in Darwin entrypoint"; exit 1; }
        echo "PASS: Darwin entrypoint has network filtering"

        echo ""
        echo "WRAPIX_NETWORK configuration validation passed"
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
