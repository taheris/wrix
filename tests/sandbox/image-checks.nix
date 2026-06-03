# Sandbox image runtime checks: verify each agent variant's image closure
# contains the expected agent runtime binary, and the `wrapix spawn` image
# load contract is idempotent against a shim podman.
{
  pkgs,
  system,
  linuxPkgs,
  fenix ? null,
  treefmt ? null,
}:

let
  inherit (pkgs) lib; # threaded for symmetry with other test imports

  shellLib = import ../../lib/util/shell.nix { };

  isLinux = lib.elem system [
    "x86_64-linux"
    "aarch64-linux"
  ];

  sandboxLib = import ../../lib/sandbox {
    inherit
      pkgs
      system
      linuxPkgs
      fenix
      treefmt
      ;
  };
  defaultImage = (sandboxLib.mkSandbox { profile = sandboxLib.profiles.base; }).image;

  defaultImageClosure = pkgs.closureInfo { rootPaths = [ defaultImage ]; };

  # Closure over the actual base-image contents (built with linuxPkgs, matching
  # lib/sandbox/image.nix). The base image itself is a compressed tarball whose
  # store references are not scannable, so the membership verifier closes over
  # the contents list the image is built from.
  baseContents = import ../../lib/sandbox/base-contents.nix { pkgs = linuxPkgs; };
  baseContentsClosure = pkgs.closureInfo { rootPaths = baseContents; };

  claudeCodePkg = linuxPkgs.claude-code;
  prekHooksBundle = import ../../lib/prek/bundle.nix { pkgs = linuxPkgs; };

  # Linux-only shim verifier for the shared `imageLoadStep` snippet (the same
  # one `wrapix spawn` runs). Asserts the skopeo-based install transport on
  # first call (per specs/sandbox.md § Image install path) and idempotence on
  # the second.
  wrapixSpawnLoadTest = pkgs.writeShellApplication {
    name = "test-wrapix-spawn-load";
    runtimeInputs = lib.optionals isLinux [
      pkgs.coreutils
      pkgs.gnugrep
    ];
    text =
      if isLinux then
        ''
          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          shim_dir="$tmp/bin"
          state="$tmp/state"
          mkdir -p "$shim_dir" "$state"
          podman_log="$state/podman.log"
          skopeo_log="$state/skopeo.log"
          : >"$podman_log"
          : >"$skopeo_log"

          IMAGE_REF="localhost/wrapix-loadtest:abc123"
          IMAGE_SOURCE="$tmp/image-source.sh"
          # The install transport pins skopeo's containers-storage destination
          # to podman's store via `podman info`; the shim reports this spec so
          # the assertion below can verify the [driver@graphroot+runroot] ref.
          STORE_SPEC="overlay@$tmp/graphroot+$tmp/runroot"

          cat >"$IMAGE_SOURCE" <<'IMG_SRC'
          #!/usr/bin/env bash
          printf 'fake-image-tarball-bytes'
          IMG_SRC
          chmod +x "$IMAGE_SOURCE"

          cat >"$shim_dir/podman" <<PODMAN_SHIM
          #!/usr/bin/env bash
          set -euo pipefail
          printf '%s\n' "\$*" >>'$podman_log'
          case "\$1" in
              image)
                  case "\$2" in
                      exists)
                          if [ -f '$state/loaded' ]; then exit 0; else exit 1; fi
                          ;;
                      *) exit 0 ;;
                  esac
                  ;;
              info)
                  printf '%s\n' '$STORE_SPEC'
                  exit 0
                  ;;
              tag)
                  : >'$state/loaded'
                  exit 0
                  ;;
              *) exit 0 ;;
          esac
          PODMAN_SHIM
          chmod +x "$shim_dir/podman"

          cat >"$shim_dir/skopeo" <<SKOPEO_SHIM
          #!/usr/bin/env bash
          set -euo pipefail
          printf '%s\n' "\$*" >>'$skopeo_log'
          exit 0
          SKOPEO_SHIM
          chmod +x "$shim_dir/skopeo"

          verbose() { :; }

          PATH="$shim_dir:$PATH"
          export PATH IMAGE_REF IMAGE_SOURCE

          ${shellLib.imageLoadStep}

          EXPECTED_DEST="containers-storage:[$STORE_SPEC]$IMAGE_REF"
          if ! grep -qF -- " $EXPECTED_DEST" "$skopeo_log"; then
              echo "first invocation did not skopeo copy oci-archive: -> $EXPECTED_DEST:" >&2
              cat "$skopeo_log" >&2
              exit 1
          fi
          if ! grep -qE 'docker-archive:[^ ]+ oci-archive:[^ ]+$' "$skopeo_log"; then
              echo "first invocation did not stage docker-archive -> oci-archive via skopeo:" >&2
              cat "$skopeo_log" >&2
              exit 1
          fi
          if ! grep -q "^tag $IMAGE_REF .*:latest$" "$podman_log"; then
              echo "first invocation did not tag $IMAGE_REF as :latest:" >&2
              cat "$podman_log" >&2
              exit 1
          fi

          : >"$podman_log"
          : >"$skopeo_log"
          ${shellLib.imageLoadStep}

          if [[ -s "$skopeo_log" ]]; then
              echo "second invocation re-invoked skopeo (install is not idempotent):" >&2
              cat "$skopeo_log" >&2
              exit 1
          fi
          if ! grep -q "^image exists $IMAGE_REF$" "$podman_log"; then
              echo "second invocation did not check 'image exists $IMAGE_REF':" >&2
              cat "$podman_log" >&2
              exit 1
          fi

          echo "test-wrapix-spawn-load: PASS"
        ''
      else
        ''
          echo "test-wrapix-spawn-load: not available on Darwin (no podman dependency on macOS)" >&2
          exit 0
        '';
  };

  # Linux-only verifier for the digest-preflight short-circuit (specs/sandbox.md
  # § Image install path; specs/image-builder.md Success Criteria #1). Drives
  # the shared `imageLoadStep` snippet through shim podman + skopeo binaries.
  # The shim records the image as content-digest-present after the first
  # install transport runs; the second `imageLoadStep` must observe the
  # digest hit and short-circuit — no skopeo copies, no tar materialization,
  # no `*-load` CLI call. Darwin's digest preflight is verified separately
  # by its own (in-progress) work in specs/sandbox.md and is skipped here.
  imageInstallDigestSkipTest = pkgs.writeShellApplication {
    name = "test-image-install-digest-skip";
    runtimeInputs = lib.optionals isLinux [
      pkgs.coreutils
      pkgs.gnugrep
    ];
    text =
      if isLinux then
        ''
          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          shim_dir="$tmp/bin"
          state="$tmp/state"
          mkdir -p "$shim_dir" "$state"
          podman_log="$state/podman.log"
          skopeo_log="$state/skopeo.log"
          image_source_log="$state/image-source.log"
          : >"$podman_log"
          : >"$skopeo_log"
          : >"$image_source_log"

          IMAGE_REF="localhost/wrapix-digestskip:abc123"
          IMAGE_SOURCE="$tmp/image-source.sh"
          IMAGE_DIGEST_PATH="$tmp/image-digest"
          DESIRED_DIGEST="sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
          printf '%s' "$DESIRED_DIGEST" >"$IMAGE_DIGEST_PATH"

          cat >"$IMAGE_SOURCE" <<IMG_SRC
          #!/usr/bin/env bash
          printf 'invoked\n' >>'$image_source_log'
          printf 'fake-image-tarball-bytes'
          IMG_SRC
          chmod +x "$IMAGE_SOURCE"

          # podman shim — logs every invocation as a single-line `$*`.
          # `image inspect --format {{.Id}} <digest>`: succeeds (exit 0,
          # echoes the digest) iff the shim has been marked installed,
          # mirroring podman's content-digest lookup.
          # `image exists <ref>`: succeeds iff installed; covers the
          # legacy ref-existence fallback.
          # `load`: recorded via $state/load-invoked so a regression to
          # an `*-load` CLI call is detectable even if the log line shape
          # changes.
          cat >"$shim_dir/podman" <<PODMAN_SHIM
          #!/usr/bin/env bash
          set -euo pipefail
          printf '%s\n' "\$*" >>'$podman_log'
          case "\$1" in
              image)
                  case "\$2" in
                      inspect)
                          if [ -f '$state/installed' ]; then
                              printf '%s\n' "\$5"
                              exit 0
                          else
                              exit 1
                          fi
                          ;;
                      exists)
                          if [ -f '$state/installed' ]; then exit 0; else exit 1; fi
                          ;;
                      *) exit 0 ;;
                  esac
                  ;;
              tag) exit 0 ;;
              load)
                  : >'$state/load-invoked'
                  exit 0
                  ;;
              *) exit 0 ;;
          esac
          PODMAN_SHIM
          chmod +x "$shim_dir/podman"

          # skopeo shim — records every invocation and marks the image
          # installed when it sees the containers-storage target (the
          # second copy of the install transport).
          cat >"$shim_dir/skopeo" <<SKOPEO_SHIM
          #!/usr/bin/env bash
          set -euo pipefail
          printf '%s\n' "\$*" >>'$skopeo_log'
          for a in "\$@"; do
              case "\$a" in
                  containers-storage:*) : >'$state/installed' ;;
              esac
          done
          exit 0
          SKOPEO_SHIM
          chmod +x "$shim_dir/skopeo"

          verbose() { :; }

          PATH="$shim_dir:$PATH"
          export PATH IMAGE_REF IMAGE_SOURCE IMAGE_DIGEST_PATH

          # First invocation: digest preflight miss → install transport runs.
          ${shellLib.imageLoadStep}

          if [[ ! -f "$state/installed" ]]; then
              echo "first invocation did not reach the install transport" >&2
              cat "$skopeo_log" >&2
              exit 1
          fi
          if ! grep -qE 'oci-archive:[^ ]+ containers-storage:'"$IMAGE_REF"'$' "$skopeo_log"; then
              echo "first invocation did not skopeo copy oci-archive: -> containers-storage:$IMAGE_REF:" >&2
              cat "$skopeo_log" >&2
              exit 1
          fi
          if ! grep -qFx -- "image inspect --format {{.Id}} $DESIRED_DIGEST" "$podman_log"; then
              echo "first invocation did not perform a digest-preflight inspect of $DESIRED_DIGEST:" >&2
              cat "$podman_log" >&2
              exit 1
          fi
          first_source_lines=$(wc -l <"$image_source_log")
          if [[ "$first_source_lines" -ne 1 ]]; then
              echo "first invocation did not materialise the image tar exactly once (got $first_source_lines):" >&2
              cat "$image_source_log" >&2
              exit 1
          fi

          : >"$podman_log"
          : >"$skopeo_log"

          # Second invocation: digest preflight hit → short-circuit.
          ${shellLib.imageLoadStep}

          if [[ -s "$skopeo_log" ]]; then
              echo "second invocation re-invoked skopeo (digest preflight did not short-circuit):" >&2
              cat "$skopeo_log" >&2
              exit 1
          fi
          if [[ -e "$state/load-invoked" ]]; then
              echo "second invocation issued a *-load CLI call (expected none):" >&2
              exit 1
          fi
          second_source_lines=$(wc -l <"$image_source_log")
          if [[ "$second_source_lines" -ne 1 ]]; then
              echo "second invocation re-materialised the image tar (lines now=$second_source_lines, expected 1):" >&2
              cat "$image_source_log" >&2
              exit 1
          fi
          if ! grep -qFx -- "image inspect --format {{.Id}} $DESIRED_DIGEST" "$podman_log"; then
              echo "second invocation did not perform a digest-preflight inspect of $DESIRED_DIGEST:" >&2
              cat "$podman_log" >&2
              exit 1
          fi
          if grep -qE '^load($| )' "$podman_log"; then
              echo "second invocation logged a podman load command (expected none):" >&2
              cat "$podman_log" >&2
              exit 1
          fi

          echo "test-image-install-digest-skip: PASS"
        ''
      else
        ''
          # Darwin's digest-preflight short-circuit lives in lib/sandbox/darwin/default.nix
          # and is exercised by its own platform-resident verifier; the shared
          # `imageLoadStep` snippet driven here is Linux-only (podman + skopeo).
          echo "test-image-install-digest-skip: skipped on this platform (Linux-only shim)" >&2
          exit 0
        '';
  };

  # Faithful verifier that the build-time `image.digest` equals the config
  # digest of the image AFTER the launcher's `docker-archive → oci-archive`
  # conversion (specs/sandbox.md § Image install path) — i.e. the value podman
  # records as `.Id` once the install transport finishes. Unlike the shim-based
  # digest-skip test (whose stub podman reports presence from a sentinel file,
  # not from the digest bytes), this drives the real skopeo conversion on the
  # same streamLayeredImage and compares against `image.digest`. skopeo
  # rewrites the config blob when converting Docker schema2 → OCI, so a digest
  # taken from the docker-archive manifest would never match what podman
  # stores, and the preflight would re-stream on every launch. This test
  # closes over the live `defaultImage` derivation and its `.digest`, so a
  # regression to the docker-archive digest is caught here.
  digestMatchesStoredIdTest = pkgs.writeShellApplication {
    name = "test-image-digest-matches-stored-id";
    runtimeInputs = lib.optionals isLinux [
      pkgs.coreutils
      pkgs.skopeo
      pkgs.jq
    ];
    text =
      if isLinux then
        ''
          HOME=$(mktemp -d); export HOME
          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp" "$HOME"' EXIT

          ${defaultImage} > "$tmp/image.tar"
          skopeo --insecure-policy copy --quiet \
            "docker-archive:$tmp/image.tar" "oci-archive:$tmp/image.oci"
          oci_digest=$(skopeo inspect --raw "oci-archive:$tmp/image.oci" | jq -r '.config.digest')
          built_digest=$(cat ${defaultImage.digest})

          if [[ "$oci_digest" != "$built_digest" ]]; then
              echo "FAIL: image.digest does not match the post-conversion OCI config digest" >&2
              echo "  built_digest (image.digest)   : $built_digest" >&2
              echo "  oci_digest   (live conversion): $oci_digest" >&2
              echo "  => launcher preflight would miss and re-stream on every launch" >&2
              exit 1
          fi

          echo "test-image-digest-matches-stored-id: PASS ($built_digest)"
        ''
      else
        ''
          # streamLayeredImage's stream script carries a Linux Python shebang;
          # the docker-archive → oci-archive conversion this verifier performs
          # is Linux-only. Darwin's digest preflight is covered separately.
          echo "test-image-digest-matches-stored-id: skipped on this platform (streamLayeredImage is Linux-only)" >&2
          exit 0
        '';
  };

  claudeRuntimeNoopTest = pkgs.writeShellApplication {
    name = "test-claude-runtime-noop";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.gnugrep
    ];
    text = ''
      closure_file=${defaultImageClosure}/store-paths
      claude_code_path=${claudeCodePkg}

      if ! grep -qxF "$claude_code_path" "$closure_file"; then
          echo "FAIL: claude-code missing from default sandbox closure" >&2
          echo "  expected: $claude_code_path" >&2
          echo "  closure : $closure_file" >&2
          exit 1
      fi

      echo "test-claude-runtime-noop: PASS"
    '';
  };

  prekHooksClosureTest = pkgs.writeShellApplication {
    name = "test-prek-hooks-closure";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.gnugrep
    ];
    text = ''
      closure_file=${defaultImageClosure}/store-paths
      prek_hooks_path=${prekHooksBundle}

      if ! grep -qxF "$prek_hooks_path" "$closure_file"; then
          echo "FAIL: prek hooks bundle not in default sandbox closure" >&2
          echo "  expected: $prek_hooks_path" >&2
          echo "  closure : $closure_file" >&2
          exit 1
      fi

      echo "test-prek-hooks-closure: PASS"
    '';
  };

  # Membership guard for the universal bottom-of-closure (specs/image-builder.md
  # § Base Image Layering). The base holds only nixpkgs-pin-dependent paths that
  # every profile actually closes over. The base profile is the minimal package
  # set (every other profile adds to it), so its image closure is the universal
  # lower bound: a base member is genuinely shared iff it is reachable there.
  # The fromImage base tar is a compressed blob whose store refs are unscannable,
  # so a member that no profile references — a profile-specific toolchain such as
  # `pkgs.rustc` (rust uses fenix's toolchain; base/python carry no Rust) or
  # `pkgs.llvmPackages.libllvm` (no profile links LLVM) — is absent from the
  # scannable image closure and is caught here as dead weight.
  baseContentsList = lib.concatStringsSep " " (map (p: ''"${p}"'') baseContents);
  baseImageUniversalTest = pkgs.writeShellApplication {
    name = "test-base-image-universal";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.gnugrep
    ];
    text = ''
      image_closure=${defaultImageClosure}/store-paths
      base_contents=${baseContentsClosure}/store-paths

      # Universality: every base member must be reachable from the base profile's
      # own packages, else it is dead weight loaded into every image's base layer.
      members=(${baseContentsList})
      for member in "''${members[@]}"; do
          if ! grep -qxF "$member" "$image_closure"; then
              echo "FAIL: wrapix-base-image member not shared by the base profile closure" >&2
              echo "  member : $member" >&2
              echo "  closure: $image_closure" >&2
              exit 1
          fi
      done

      # Exclusion: no profile references rustc or libllvm, so neither is a base member.
      for toolchain in ${linuxPkgs.rustc} ${linuxPkgs.llvmPackages.libllvm}; do
          if grep -qxF "$toolchain" "$base_contents"; then
              echo "FAIL: profile-specific toolchain present in wrapix-base-image contents" >&2
              echo "  unexpected: $toolchain" >&2
              exit 1
          fi
      done

      echo "test-base-image-universal: PASS"
    '';
  };

  # Hash-stability guard for `wrapix-base-image` (specs/image-builder.md
  # § Base Image Layering). The base captures only the nixpkgs-pin-dependent
  # bottom-of-closure, so its derivation hash must not move when any
  # profile-level input changes. We build the full sandbox image under several
  # perturbations of profile.packages, profile.env, MCP configs, the merged
  # Claude settings JSON, and the agent runtime selection, then read each
  # image's chained `fromImage` base (image.baseImage) and assert one drvPath
  # across all of them. A regression that threads a profile input into
  # base-image.nix would split the drvPaths and fail here.
  baseImageOf = args: (sandboxLib.mkSandbox args).image.baseImage.drvPath;
  referenceBaseImage = baseImageOf { profile = sandboxLib.profiles.base; };
  baseImagePermutations = {
    packages = baseImageOf {
      profile = sandboxLib.profiles.base;
      packages = [ linuxPkgs.hello ];
    };
    env = baseImageOf {
      profile = sandboxLib.profiles.base;
      env = {
        WRAPIX_HASH_STABLE_PROBE = "v2";
      };
    };
    mcpConfigs = baseImageOf {
      profile = sandboxLib.profiles.base;
      mcpRuntime = true;
    };
    claudeSettings = baseImageOf {
      profile = sandboxLib.profiles.base;
      model = "claude-hash-stable-probe";
    };
    agentDirect = baseImageOf {
      profile = sandboxLib.profiles.base;
      agent = "direct";
      directRunner = linuxPkgs.hello;
    };
    profilePython = baseImageOf { profile = sandboxLib.profiles.python; };
  };
  baseImageHashStableChecks = lib.concatStringsSep "\n" (
    lib.mapAttrsToList (name: drvPath: ''
      if [[ "${drvPath}" != "$reference" ]]; then
          echo "FAIL: wrapix-base-image drvPath changed under profile perturbation '${name}'" >&2
          echo "  reference  : $reference" >&2
          echo "  ${name}: ${drvPath}" >&2
          status=1
      fi
    '') baseImagePermutations
  );
  baseImageHashStableTest = pkgs.writeShellApplication {
    name = "test-base-image-hash-stable";
    runtimeInputs = [ pkgs.coreutils ];
    text = ''
      reference="${referenceBaseImage}"
      status=0

      ${baseImageHashStableChecks}

      if [[ "$status" -ne 0 ]]; then
          exit 1
      fi

      echo "test-base-image-hash-stable: PASS (base drvPath invariant across ${toString (builtins.length (builtins.attrNames baseImagePermutations))} profile perturbations)"
    '';
  };

  # Hash-stability guard for `wrapix-stable-profile-<name>` (tier 1;
  # specs/image-builder.md § Provenance-Tiered Layering). Tier 1's contents are
  # fixed per profile instance, so its derivation hash must not move when a
  # tier-2 input changes. We read each sandbox image's chained `fromImage`
  # tier-1 (image.stableProfileImage) under several tier-2 perturbations — a
  # downstream-appended package, the agent runtime selection, the merged Claude
  # settings JSON, and MCP configs — holding the profile fixed, then assert one
  # drvPath across all of them. A regression threading a tier-2 input into
  # stable-profile-image.nix would split the drvPaths and fail here.
  stableProfileImageOf = args: (sandboxLib.mkSandbox args).image.stableProfileImage.drvPath;
  referenceStableProfile = stableProfileImageOf { profile = sandboxLib.profiles.base; };
  stableProfilePermutations = {
    packages = stableProfileImageOf {
      profile = sandboxLib.profiles.base;
      packages = [ linuxPkgs.hello ];
    };
    agentDirect = stableProfileImageOf {
      profile = sandboxLib.profiles.base;
      agent = "direct";
      directRunner = linuxPkgs.hello;
    };
    claudeSettings = stableProfileImageOf {
      profile = sandboxLib.profiles.base;
      model = "claude-hash-stable-probe";
    };
    mcpConfigs = stableProfileImageOf {
      profile = sandboxLib.profiles.base;
      mcpRuntime = true;
    };
  };
  stableProfileHashStableChecks = lib.concatStringsSep "\n" (
    lib.mapAttrsToList (name: drvPath: ''
      if [[ "${drvPath}" != "$reference" ]]; then
          echo "FAIL: wrapix-stable-profile drvPath changed under tier-2 perturbation '${name}'" >&2
          echo "  reference  : $reference" >&2
          echo "  ${name}: ${drvPath}" >&2
          status=1
      fi
    '') stableProfilePermutations
  );
  stableProfileHashStableTest = pkgs.writeShellApplication {
    name = "test-stable-profile-hash-stable";
    runtimeInputs = [ pkgs.coreutils ];
    text = ''
      reference="${referenceStableProfile}"
      status=0

      ${stableProfileHashStableChecks}

      if [[ "$status" -ne 0 ]]; then
          exit 1
      fi

      echo "test-stable-profile-hash-stable: PASS (tier-1 drvPath invariant across ${toString (builtins.length (builtins.attrNames stableProfilePermutations))} tier-2 perturbations)"
    '';
  };

  # Membership guard for tier 1 (specs/image-builder.md § Provenance-Tiered
  # Layering): `wrapix-stable-profile-<name>` holds only fixed-per-instance
  # content. A downstream-appended package and a consumer-supplied agent runtime
  # are tier-2, so neither may appear in tier 1's `lowerTiersClosure` (the union
  # the leaf strips), yet both must ship in the leaf image's own closure. Built
  # via image.nix directly with a light entrypointPkg so the check does not drag
  # in claude-code.
  membershipAppendedPkg = pkgs.writeShellScriptBin "wrapix-stable-membership-appended" ''
    echo "downstream-appended package"
  '';
  membershipRunnerPkg = pkgs.writeShellScriptBin "wrapix-stable-membership-runner" ''
    echo "consumer-supplied agent runtime"
  '';
  membershipImage = import ../../lib/sandbox/image.nix {
    inherit pkgs;
    asTarball = !isLinux;
    profile = {
      name = "membership";
      corePackages = [ pkgs.coreutils ];
      packages = [
        pkgs.coreutils
        membershipAppendedPkg
      ];
      env = { };
    };
    agent = "direct";
    directRunner = membershipRunnerPkg;
    entrypointPkg = pkgs.hello;
    entrypointSh = ../../lib/sandbox/linux/entrypoint.sh;
    claudeConfig = { };
    claudeSettings = { };
  };
  membershipImageClosure = pkgs.closureInfo { rootPaths = [ membershipImage ]; };
  stableProfileMembershipTest = pkgs.writeShellApplication {
    name = "test-stable-profile-membership";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.gnugrep
    ];
    text = ''
      tier1_closure=${membershipImage.stableProfileImage.lowerTiersClosure}/store-paths
      image_closure=${membershipImageClosure}/store-paths
      appended=${membershipAppendedPkg}
      runner=${membershipRunnerPkg}

      for tier2 in "$appended" "$runner"; do
          if grep -qxF "$tier2" "$tier1_closure"; then
              echo "FAIL: tier-2 path leaked into the wrapix-stable-profile tier-1 closure" >&2
              echo "  leaked : $tier2" >&2
              echo "  tier-1 : $tier1_closure" >&2
              exit 1
          fi
          if ! grep -qxF "$tier2" "$image_closure"; then
              echo "FAIL: tier-2 path missing from the leaf image closure" >&2
              echo "  expected: $tier2" >&2
              echo "  image   : $image_closure" >&2
              exit 1
          fi
      done

      echo "test-stable-profile-membership: PASS (appended package + agent runtime are tier-2 leaf, absent from tier 1)"
    '';
  };

  # Pinned-toolchain tier guard (specs/image-builder.md § Provenance-Tiered
  # Layering): a downstream-pinned rust toolchain is fixed per instance, so it
  # belongs to `corePackages` and lands in tier 1, never the volatile leaf. We
  # build a project-pinned rust profile from a fixture rust-toolchain.toml and
  # assert every rust-specific core package (the pinned toolchain plus its fixed
  # support packages) is reachable from the leaf image's tier-1
  # `lowerTiersClosure`. Skipped when the fenix input is absent (no rust profile).
  toolchainFixtureSha = "sha256-SXRtAuO4IqNOQq+nLbrsDFbVk+3aVA8NNpSZsKlVH/8=";
  pinnedToolchainStableTest =
    if fenix == null then
      pkgs.writeShellApplication {
        name = "test-pinned-toolchain-stable-tier";
        runtimeInputs = [ pkgs.coreutils ];
        text = ''
          echo "test-pinned-toolchain-stable-tier: skipped (fenix input absent, no rust profile)" >&2
          exit 0
        '';
      }
    else
      let
        pinnedProfile = sandboxLib.rustProfileFromFile {
          file = ../fixtures/rust-toolchain.toml;
          sha256 = toolchainFixtureSha;
        };
        pinnedSandbox = sandboxLib.mkSandbox { profile = pinnedProfile; };
        rustCoreExtras = lib.subtractLists sandboxLib.profiles.base.corePackages pinnedProfile.corePackages;
        rustCoreExtrasList = lib.concatStringsSep " " (map (p: ''"${p}"'') rustCoreExtras);
      in
      pkgs.writeShellApplication {
        name = "test-pinned-toolchain-stable-tier";
        runtimeInputs = [
          pkgs.coreutils
          pkgs.gnugrep
        ];
        text = ''
          tier1_closure=${pinnedSandbox.image.stableProfileImage.lowerTiersClosure}/store-paths

          members=(${rustCoreExtrasList})
          if [[ "''${#members[@]}" -eq 0 ]]; then
              echo "FAIL: pinned rust profile contributed no core packages over base" >&2
              exit 1
          fi

          for member in "''${members[@]}"; do
              if ! grep -qxF "$member" "$tier1_closure"; then
                  echo "FAIL: pinned-toolchain core package absent from the wrapix-stable-profile tier-1 closure" >&2
                  echo "  member : $member" >&2
                  echo "  tier-1 : $tier1_closure" >&2
                  exit 1
              fi
          done

          echo "test-pinned-toolchain-stable-tier: PASS (''${#members[@]} pinned core packages land in tier 1)"
        '';
      };

  # Leaf-only-change guard (specs/image-builder.md § Provenance-Tiered Layering;
  # Non-Functional § Iteration cost). Two leaf images that differ only in a
  # tier-2 input — the agent runtime selection — share an identical
  # `wrapix-stable-profile-<name>` (tier 1) and `wrapix-base-image` (tier 0),
  # since neither depends on the agent axis. Every tier-0 and tier-1 layer blob
  # must therefore be byte-identical across the two leaves' manifests; only
  # leaf-tier blobs change. Built via image.nix directly with a light
  # entrypointPkg so the check does not drag in claude-code.
  mkLeafAgentProbe =
    {
      agent,
      directRunner ? null,
    }:
    import ../../lib/sandbox/image.nix {
      inherit pkgs agent directRunner;
      profile = {
        name = "leafprobe";
        corePackages = [ pkgs.coreutils ];
        packages = [ pkgs.coreutils ];
        env = { };
      };
      entrypointPkg = pkgs.hello;
      entrypointSh = ../../lib/sandbox/linux/entrypoint.sh;
      claudeConfig = { };
      claudeSettings = { };
    };
  leafProbeClaude = mkLeafAgentProbe { agent = "claude"; };
  leafProbeDirect = mkLeafAgentProbe {
    agent = "direct";
    directRunner = pkgs.writeShellScriptBin "wrapix-leaf-probe-runner" ''
      echo "leaf probe direct runner"
    '';
  };
  downstreamChangeLeafOnlyTest = pkgs.writeShellApplication {
    name = "test-downstream-change-leaf-only";
    runtimeInputs = lib.optionals isLinux [
      pkgs.coreutils
      pkgs.gnutar
      pkgs.jq
    ];
    text =
      if isLinux then
        ''
          imgA=${leafProbeClaude}
          imgB=${leafProbeDirect}
          tier1_tar=${leafProbeClaude.stableProfileImage}

          if [[ "$imgA" == "$imgB" ]]; then
              echo "FAIL: claude/direct probe images resolved to the same stream script;" >&2
              echo "      the agent-runtime perturbation did not materialise a distinct leaf" >&2
              exit 1
          fi

          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          # streamLayeredImage emits a docker-archive tar; manifest.json lists
          # each layer blob as "<sha256>/layer.tar".
          "$imgA" | tar -xO manifest.json > "$tmp/a.json"
          "$imgB" | tar -xO manifest.json > "$tmp/b.json"
          # GNU tar auto-detects the tier-1 buildLayeredImage tar's compression.
          tar -xOf "$tier1_tar" manifest.json > "$tmp/tier1.json"

          jq -r '.[0].Layers[]' "$tmp/a.json" | sort -u > "$tmp/a.layers"
          jq -r '.[0].Layers[]' "$tmp/b.json" | sort -u > "$tmp/b.layers"
          jq -r '.[0].Layers[]' "$tmp/tier1.json" | sort -u > "$tmp/tier1.layers"

          comm -12 "$tmp/a.layers" "$tmp/b.layers" > "$tmp/shared.layers"
          tier1_count=$(wc -l < "$tmp/tier1.layers")

          # Every tier-0 + tier-1 blob (the whole tier-1 fromImage manifest) must
          # survive byte-identical in BOTH leaves, i.e. appear in their shared set.
          missing=$(comm -23 "$tmp/tier1.layers" "$tmp/shared.layers")
          if [[ -n "$missing" ]]; then
              echo "FAIL: a tier-0/tier-1 layer blob changed across the agent-runtime perturbation" >&2
              echo "  tier blobs not shared by both leaves:" >&2
              echo "$missing" >&2
              exit 1
          fi

          only_a=$(comm -23 "$tmp/a.layers" "$tmp/b.layers" | wc -l)
          only_b=$(comm -13 "$tmp/a.layers" "$tmp/b.layers" | wc -l)
          if [[ "$only_a" -eq 0 && "$only_b" -eq 0 ]]; then
              echo "FAIL: no leaf-tier blob changed; the perturbation left the manifest identical" >&2
              exit 1
          fi

          echo "test-downstream-change-leaf-only: PASS ($tier1_count tier-0/tier-1 blobs byte-identical; only_in_A=$only_a only_in_B=$only_b leaf-only)"
        ''
      else
        ''
          # streamLayeredImage's stream script carries a Linux Python shebang;
          # the manifest diff this verifier performs is Linux-only. Darwin's
          # iteration-cost bound is the tiered chain alone (specs/image-builder.md
          # § Out of Scope).
          echo "test-downstream-change-leaf-only: skipped on this platform (streamLayeredImage is Linux-only)" >&2
          exit 0
        '';
  };

  # Iteration-cost-bounded probe (specs/image-builder.md Success Criteria:
  # "A one-file perturbation in profile-level inputs (one wrapper script touched)
  # leaves every layer-blob hash in the resulting image's manifest unchanged
  # except for the customisation layer and any top layer that directly depends on
  # the changed file"). Two profile images that differ only in a single wrapper
  # script's body — a one-line edit. The fromImage base, the entrypoint, and
  # every other input are held fixed, so the streamLayeredImage manifests must
  # share every layer blob except the customisation layer (which always re-hashes,
  # since it aggregates the perturbed content) and the top layer(s) carrying the
  # wrapper's store path.
  mkIterationProbeImage =
    line:
    import ../../lib/sandbox/image.nix {
      inherit pkgs;
      profile = {
        name = "iterprobe";
        packages = [
          (pkgs.writeShellScriptBin "wrapix-iteration-probe" ''
            # iteration-cost-bounded probe wrapper
            echo "iteration probe: ${line}"
          '')
        ];
        env = { };
      };
      entrypointPkg = pkgs.hello;
      entrypointSh = ../../lib/sandbox/linux/entrypoint.sh;
      claudeConfig = { };
      claudeSettings = { };
    };
  iterationProbeImageA = mkIterationProbeImage "alpha";
  iterationProbeImageB = mkIterationProbeImage "beta";

  # Changed-blob ceiling: the customisation layer plus the top layer(s) carrying
  # the perturbed wrapper's store path. A regression that threaded the wrapper
  # into a shared bottom-of-closure layer would churn base blobs and exceed it.
  iterationMaxChangedBlobs = 4;

  iterationCostBoundedTest = pkgs.writeShellApplication {
    name = "test-iteration-cost-bounded";
    runtimeInputs = lib.optionals isLinux [
      pkgs.coreutils
      pkgs.gnutar
      pkgs.jq
    ];
    text =
      if isLinux then
        ''
          imgA=${iterationProbeImageA}
          imgB=${iterationProbeImageB}
          max_changed=${toString iterationMaxChangedBlobs}

          if [[ "$imgA" == "$imgB" ]]; then
              echo "FAIL: probe images A and B resolved to the same stream script;" >&2
              echo "      the one-line wrapper perturbation did not materialise a distinct image" >&2
              exit 1
          fi

          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          # streamLayeredImage emits a docker-archive tar; manifest.json lists each
          # layer blob as "<sha256>/layer.tar".
          "$imgA" | tar -xO manifest.json > "$tmp/a.json"
          "$imgB" | tar -xO manifest.json > "$tmp/b.json"

          jq -r '.[0].Layers[]' "$tmp/a.json" | sort -u > "$tmp/a.layers"
          jq -r '.[0].Layers[]' "$tmp/b.json" | sort -u > "$tmp/b.layers"

          # The customisation layer is streamLayeredImage's final layer.
          custA=$(jq -r '.[0].Layers[-1]' "$tmp/a.json")
          custB=$(jq -r '.[0].Layers[-1]' "$tmp/b.json")

          total=$(wc -l < "$tmp/a.layers")
          shared=$(comm -12 "$tmp/a.layers" "$tmp/b.layers" | wc -l)
          only_a=$(comm -23 "$tmp/a.layers" "$tmp/b.layers" | wc -l)
          only_b=$(comm -13 "$tmp/a.layers" "$tmp/b.layers" | wc -l)

          echo "layer blobs: total(A)=$total shared=$shared only_in_A=$only_a only_in_B=$only_b" >&2

          # The customisation layer aggregates the perturbed content, so it must move.
          if [[ "$custA" == "$custB" ]]; then
              echo "FAIL: customisation layer blob unchanged under the wrapper perturbation ($custA)" >&2
              echo "      the manifest diff cannot be attributed to the changed file" >&2
              exit 1
          fi

          if [[ "$only_a" -gt "$max_changed" || "$only_b" -gt "$max_changed" ]]; then
              echo "FAIL: one-file perturbation changed more than $max_changed layer blobs" >&2
              echo "      only_in_A=$only_a only_in_B=$only_b (expected <= $max_changed:" >&2
              echo "      customisation layer + the top layer(s) carrying the wrapper)" >&2
              echo "  only in A:" >&2
              comm -23 "$tmp/a.layers" "$tmp/b.layers" >&2
              echo "  only in B:" >&2
              comm -13 "$tmp/a.layers" "$tmp/b.layers" >&2
              exit 1
          fi

          if [[ "$shared" -lt $((total - max_changed)) ]]; then
              echo "FAIL: too few shared layer blobs ($shared of $total); the base and" >&2
              echo "      sibling layers should be invariant under a one-file perturbation" >&2
              exit 1
          fi

          echo "test-iteration-cost-bounded: PASS ($shared/$total layer blobs identical; only_in_A=$only_a only_in_B=$only_b within bound $max_changed)"
        ''
      else
        ''
          # streamLayeredImage's stream script carries a Linux Python shebang;
          # the manifest diff this verifier performs is Linux-only. Darwin's
          # iteration-cost bound is covered by base-image pinning alone (see
          # specs/image-builder.md § Out of Scope).
          echo "test-iteration-cost-bounded: skipped on this platform (streamLayeredImage is Linux-only)" >&2
          exit 0
        '';
  };

  # Customisation-layer-bounded probe. Opening the in-image Nix store
  # read-write (includeNixDB + the prekHooksBundle load-db in image.nix)
  # creates Nix's gc-reserved-space file `nix/var/nix/db/reserved` — an 8 MiB
  # all-zero block that landed in the streamLayeredImage customisation layer
  # and re-hashed it on every profile-level input change. image.nix removes
  # the file at the end of extraCommands; Nix recreates it lazily in-container.
  # This asserts the removal holds: the customisation layer must not carry
  # `db/reserved`, and its uncompressed tar must stay under the bound the 8 MiB
  # block used to blow past.
  customisationLayerMaxBytes = 4 * 1024 * 1024;
  customisationLayerBoundedTest = pkgs.writeShellApplication {
    name = "test-customisation-layer-bounded";
    runtimeInputs = lib.optionals isLinux [
      pkgs.coreutils
      pkgs.gnutar
      pkgs.gnugrep
      pkgs.jq
    ];
    text =
      if isLinux then
        ''
          img=${iterationProbeImageA}
          max_bytes=${toString customisationLayerMaxBytes}

          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          "$img" | tar -xf - -C "$tmp"

          cust=$(jq -r '.[0].Layers[-1]' "$tmp/manifest.json")
          layer="$tmp/$cust"

          if [[ ! -f "$layer" ]]; then
              echo "FAIL: customisation layer tar $cust not found in image archive" >&2
              exit 1
          fi

          if tar -tf "$layer" | grep -qE '(^|/)nix/var/nix/db/reserved$'; then
              echo "FAIL: customisation layer still carries nix/var/nix/db/reserved" >&2
              echo "      (the gc-reserved-space padding was not elided)" >&2
              exit 1
          fi

          layer_bytes=$(stat -c %s "$layer")
          echo "customisation layer: $cust ($layer_bytes bytes)" >&2

          if [[ "$layer_bytes" -gt "$max_bytes" ]]; then
              echo "FAIL: customisation layer is $layer_bytes bytes (bound $max_bytes)" >&2
              echo "      reserved padding elision should keep it well under the bound" >&2
              exit 1
          fi

          echo "test-customisation-layer-bounded: PASS ($layer_bytes bytes, reserved absent)"
        ''
      else
        ''
          # streamLayeredImage's stream script carries a Linux Python shebang;
          # extracting the customisation layer here is Linux-only. The reserved
          # elision itself runs identically under buildLayeredImage on Darwin.
          echo "test-customisation-layer-bounded: skipped on this platform (streamLayeredImage is Linux-only)" >&2
          exit 0
        '';
  };

  # Nix-DB-consistency verifier (specs/image-builder.md § In-Container Nix Store
  # Consistency; Success Criteria "The baked image's Nix database is consistent
  # with its on-disk store"). The image must register its FULL on-disk closure
  # valid — the leaf's own contents AND every path the fromImage base +
  # stable-profile tiers physically lay down (notably the tier-1 generated
  # passwd/group/nix.conf store paths and prekHooksBundle). includeNixDB alone
  # registers only the leaf's `contents`, leaving those tier paths on disk but
  # unregistered — orphans that make the unprivileged in-container user's
  # additive Nix ops try to chmod a root-owned path and fail with EPERM.
  #
  # The test reconstructs the composed image's on-disk store by listing every
  # store path across all three tiers' layer tars, then reads the baked
  # db.sqlite (carried in the leaf customisation layer) and asserts no on-disk
  # store path is missing from ValidPaths. Built via image.nix directly with a
  # light entrypointPkg so the check does not drag in claude-code, but through
  # the same tier chain that produces the diagnosed orphan.
  nixDbProbeImage = import ../../lib/sandbox/image.nix {
    inherit pkgs;
    profile = {
      name = "nixdbprobe";
      corePackages = [ pkgs.coreutils ];
      packages = [ pkgs.coreutils ];
      env = { };
    };
    entrypointPkg = pkgs.hello;
    entrypointSh = ../../lib/sandbox/linux/entrypoint.sh;
    claudeConfig = { };
    claudeSettings = { };
  };
  imageNixDbConsistentTest = pkgs.writeShellApplication {
    name = "test-image-nix-db-consistent";
    runtimeInputs = lib.optionals isLinux [
      pkgs.coreutils
      pkgs.gnutar
      pkgs.gnugrep
      pkgs.jq
      pkgs.sqlite
    ];
    text =
      if isLinux then
        ''
          leaf_stream=${nixDbProbeImage}
          tier1_tar=${nixDbProbeImage.stableProfileImage}
          tier0_tar=${nixDbProbeImage.baseImage}

          tmp=$(mktemp -d)
          trap 'rm -rf "$tmp"' EXIT

          "$leaf_stream" > "$tmp/leaf.tar"

          # Unpack each tier's docker-archive (manifest.json + <sha>/layer.tar
          # blobs) and list the store paths every layer blob carries. The union
          # across all three tiers is the composed image's on-disk store.
          unpack_dir="$tmp/archives"
          mkdir -p "$unpack_dir"
          list_tier_store_paths() {
              local arc="$1" d
              d=$(mktemp -d -p "$unpack_dir")
              tar -xf "$arc" -C "$d"
              local layer
              while IFS= read -r layer; do
                  tar -tf "$d/$layer"
              done < <(jq -r '.[0].Layers[]' "$d/manifest.json")
          }

          {
              list_tier_store_paths "$tmp/leaf.tar"
              list_tier_store_paths "$tier1_tar"
              list_tier_store_paths "$tier0_tar"
          } | grep -oE 'nix/store/[a-z0-9]{32}-[^/]+' | sort -u | sed 's#^#/#' \
              > "$tmp/ondisk"

          # The baked Nix DB rides in the leaf customisation layer; find the
          # layer carrying it and extract the whole db dir (the sqlite store is
          # WAL-mode, so the -wal/-shm sidecars must travel with db.sqlite for
          # the registered rows to be visible).
          leafx="$tmp/leafx"
          mkdir -p "$leafx"
          tar -xf "$tmp/leaf.tar" -C "$leafx"
          dbroot="$tmp/dbroot"
          mkdir -p "$dbroot"
          db=""
          while IFS= read -r layer; do
              if tar -tf "$leafx/$layer" | grep -qE '(^|\./)nix/var/nix/db/db\.sqlite$'; then
                  tar -xf "$leafx/$layer" -C "$dbroot"
                  db="$dbroot/nix/var/nix/db/db.sqlite"
                  break
              fi
          done < <(jq -r '.[0].Layers[]' "$leafx/manifest.json")

          if [[ -z "$db" || ! -f "$db" ]]; then
              echo "FAIL: no nix/var/nix/db/db.sqlite found in any leaf layer" >&2
              echo "      (includeNixDB / load-db did not bake a database)" >&2
              exit 1
          fi

          # Store paths are extracted read-only; SQLite opened read-write needs
          # to write the -wal/-shm sidecars while reading WAL-resident rows.
          chmod -R u+rwX "$dbroot/nix/var/nix/db"
          sqlite3 "$db" 'SELECT path FROM ValidPaths' | sort -u \
              > "$tmp/registered"

          ondisk_count=$(wc -l < "$tmp/ondisk")
          registered_count=$(wc -l < "$tmp/registered")
          if [[ "$ondisk_count" -eq 0 ]]; then
              echo "FAIL: reconstructed on-disk store is empty; the test did not" >&2
              echo "      enumerate the image's store paths" >&2
              exit 1
          fi
          if [[ "$registered_count" -eq 0 ]]; then
              echo "FAIL: baked Nix DB registered no ValidPaths" >&2
              exit 1
          fi

          # Every on-disk store path must be registered valid — no orphan.
          orphans=$(comm -23 "$tmp/ondisk" "$tmp/registered")
          if [[ -n "$orphans" ]]; then
              orphan_count=$(printf '%s\n' "$orphans" | grep -c '^' || true)
              echo "FAIL: $orphan_count on-disk store path(s) are not registered valid" >&2
              echo "      in the baked Nix DB (orphans break additive in-container Nix):" >&2
              printf '%s\n' "$orphans" | sed 's/^/  /' >&2
              exit 1
          fi

          echo "test-image-nix-db-consistent: PASS ($ondisk_count on-disk store paths all registered valid; no orphan)"
        ''
      else
        ''
          # streamLayeredImage's stream script carries a Linux Python shebang;
          # reconstructing the on-disk store from the leaf docker-archive here
          # is Linux-only. The full-closure registration in image.nix runs
          # identically under buildLayeredImage on Darwin.
          echo "test-image-nix-db-consistent: skipped on this platform (streamLayeredImage is Linux-only)" >&2
          exit 0
        '';
  };

in
{
  inherit
    wrapixSpawnLoadTest
    imageInstallDigestSkipTest
    digestMatchesStoredIdTest
    claudeRuntimeNoopTest
    prekHooksClosureTest
    baseImageUniversalTest
    baseImageHashStableTest
    stableProfileHashStableTest
    stableProfileMembershipTest
    pinnedToolchainStableTest
    downstreamChangeLeafOnlyTest
    iterationCostBoundedTest
    customisationLayerBoundedTest
    imageNixDbConsistentTest
    ;
}
