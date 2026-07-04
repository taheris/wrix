{ pkgs, system, ... }:

let
  inherit (pkgs.lib) escapeShellArg;

  ciApp = app: ''
    local app=${escapeShellArg app}
    local build_dir
    local root
    local runner
    local status
    root="$(repo_root)"
    build_dir="$(mktemp -d -t wrix-verify-linux-builder.XXXXXX)"
    nix build --out-link "$build_dir/result" --no-warn-dirty "$root#legacyPackages.${system}.ciApps.$app"
    runner="$(readlink -f "$build_dir/result")"
    status=0
    "$runner/bin/$app" || status="$?"
    rm -rf "$build_dir"
    return "$status"
  '';

  builderScript = function: ''
    run_repo_script ${escapeShellArg "tests/builder/key-material.sh"} ${escapeShellArg function}
  '';
in
{
  "linux-builder.integration" = ''
    run_repo_script ${escapeShellArg "tests/standalone/builder-test.sh"}
  '';

  "linux-builder.sshd-hardening" = ciApp "test-linux-builder-sshd-hardening";

  "linux-builder.key-material-generation" = builderScript "test_generates_per_user_ed25519_material";

  "linux-builder.key-material-idempotent" = builderScript "test_preserves_existing_private_keys";

  "linux-builder.image-source-kind" = ciApp "test-linux-builder-image-source-kind";

  "linux-builder.source-kind-load-transport" =
    builderScript "test_loads_image_through_source_kind_contract";
}
