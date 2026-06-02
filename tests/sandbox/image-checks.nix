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

          if ! grep -qE 'oci-archive:[^ ]+ containers-storage:'"$IMAGE_REF"'$' "$skopeo_log"; then
              echo "first invocation did not skopeo copy oci-archive: -> containers-storage:$IMAGE_REF:" >&2
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
    iterationCostBoundedTest
    customisationLayerBoundedTest
    ;
}
