{ pkgs, linuxPkgs, ... }:

let
  inherit (pkgs.lib) escapeShellArg;

  fixtureBuilder = import ../builder/fixture.nix { inherit pkgs linuxPkgs; };
  builderScript = function: ''
    WRIX_BUILDER_BIN=${escapeShellArg "${fixtureBuilder}/bin/wrix-builder"} \
      run_repo_script ${escapeShellArg "tests/builder/key-material.sh"} ${escapeShellArg function}
  '';
in
{
  "linux-builder.integration" = ''
    local platform
    local version
    local major
    local status=0

    platform="$(uname -s)"
    if [[ "$platform" == "Darwin" ]]; then
      version="$(sw_vers -productVersion)"
      major="''${version%%.*}"
      if [[ "$major" =~ ^[0-9]+$ && "$major" -ge 26 ]]; then
        run_repo_script ${escapeShellArg "tests/standalone/builder-test.sh"}
        return
      fi
    fi

    run_repo_script ${escapeShellArg "tests/standalone/builder-test.sh"} || status="$?"
    if [[ "$status" -ne 77 ]]; then
      fail "builder integration exited $status; expected unsupported-platform skip"
    fi
    return 77
  '';

  "linux-builder.key-material-generation" = builderScript "test_generates_per_user_ed25519_material";

  "linux-builder.key-material-idempotent" = builderScript "test_preserves_existing_private_keys";

}
