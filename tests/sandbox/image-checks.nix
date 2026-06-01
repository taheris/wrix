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

in
{
  inherit
    wrapixSpawnLoadTest
    claudeRuntimeNoopTest
    prekHooksClosureTest
    ;
}
