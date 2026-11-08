{
  pkgs,
  system,
  linuxPkgs,
  runtimeEnvironmentTests,
  ...
}:
let
  inherit (import ../lib/verifier.nix) live;
  fixture = import ../sandbox/runtime-environment-image.nix { pkgs = linuxPkgs; };
in
{
  "security.pi-auth-storage".script = ''
    nix build --no-link ".#checks.${system}.pi-auth-storage"
  '';

  "security.runtime-environment-native" =
    (live ''
      (
      local root image_ref
      root="$(mktemp -d)"
      trap 'rm -rf "$root"' EXIT
      chmod 0700 "$root"
      ${
        if pkgs.stdenv.hostPlatform.isDarwin then
          ''
            ${pkgs.skopeo}/bin/skopeo --tmpdir "$root" --insecure-policy copy \
              docker-archive:${fixture} "oci-archive:$root/image.tar"
            image_ref="$(container image load --input "$root/image.tar")"
          ''
        else
          ''
            podman load --input ${fixture}
            image_ref='${fixture.imageName}:${fixture.imageTag}'
          ''
      }
      export WRIX_TEST_RUNTIME_ENV_IMAGE="$image_ref"
      export HOME="$root/home" XDG_CONFIG_HOME="$root/config" XDG_STATE_HOME="$root/state"
      export XDG_CACHE_HOME="$root/cache" XDG_DATA_HOME="$root/data" XDG_RUNTIME_DIR="$root/run"
      mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME" "$XDG_DATA_HOME" "$XDG_RUNTIME_DIR"
      ${runtimeEnvironmentTests}/bin/runtime-environment-tests --ignored --exact \
        actual_native_guest_consumes_the_launchers_private_environment_without_host_argv_values
      )
    '')
    // {
      inputs = (import ../lib/image-resources.nix { }).imageInputs;
    };
}
