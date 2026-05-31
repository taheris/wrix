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

  # Linux-only shim-podman verifier for the shared `imageLoadStep` snippet
  # (the same one `wrapix spawn` runs). Asserts load on first call,
  # idempotence on the second.
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
          log_file="$state/podman.log"
          : >"$log_file"

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
          printf '%s\n' "\$*" >>'$log_file'
          case "\$1" in
              image)
                  case "\$2" in
                      exists)
                          if [ -f '$state/loaded' ]; then exit 0; else exit 1; fi
                          ;;
                      *) exit 0 ;;
                  esac
                  ;;
              load)
                  cat >'$state/load-stdin' || true
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

          verbose() { :; }

          PATH="$shim_dir:$PATH"
          export PATH IMAGE_REF IMAGE_SOURCE

          ${shellLib.imageLoadStep}

          if ! grep -q '^load -q' "$log_file"; then
              echo "first invocation did not call 'podman load':" >&2
              cat "$log_file" >&2
              exit 1
          fi
          if ! grep -q "^tag .*:latest $IMAGE_REF$" "$log_file"; then
              echo "first invocation did not tag image as $IMAGE_REF:" >&2
              cat "$log_file" >&2
              exit 1
          fi

          : >"$log_file"
          ${shellLib.imageLoadStep}

          if grep -q '^load -q' "$log_file"; then
              echo "second invocation re-loaded image (load is not idempotent):" >&2
              cat "$log_file" >&2
              exit 1
          fi
          if ! grep -q "^image exists $IMAGE_REF$" "$log_file"; then
              echo "second invocation did not check 'image exists $IMAGE_REF':" >&2
              cat "$log_file" >&2
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
