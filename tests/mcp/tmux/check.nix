# tmux-mcp tests - shellcheck the integration scripts.
#
# The crate's clippy + nextest land in tests/default.nix via
# wrapix.tmuxMcpPackage. The live integration suite (test_*.sh + e2e/) is
# driven host-side by tests/mcp/tmux/{integration,e2e-sandbox}.sh, which
# the loom gate dispatches per criterion; nothing here boots a VM.
{
  pkgs,
  src,
  ...
}:

let
  inherit (pkgs) bash runCommandLocal;
in
{
  # Verify integration test shell scripts have valid syntax.
  tmux-mcp-integration-syntax =
    runCommandLocal "tmux-mcp-integration-syntax"
      {
        nativeBuildInputs = [
          bash
          pkgs.shellcheck
        ];
      }
      ''
        echo "Checking integration test script syntax..."

        INTEGRATION_DIR="${src}/tests/mcp/tmux"
        echo "Checking integration test scripts in $INTEGRATION_DIR..."
        for script in "$INTEGRATION_DIR"/*.sh; do
          if [ -f "$script" ]; then
            echo "Checking syntax: $(basename "$script")"
            bash -n "$script"
          fi
        done

        # Run shellcheck on integration test scripts.
        # SC1091: Can't follow non-constant source (paths differ in nix store)
        # SC2034: Variable appears unused (some are used in sourced test_lib.sh or set for external use)
        echo "Running shellcheck on integration test scripts..."
        find "$INTEGRATION_DIR" -maxdepth 1 -name '*.sh' -exec shellcheck -x --exclude=SC1091,SC2034 {} +

        E2E_DIR="${src}/tests/mcp/tmux/e2e"
        if [ -d "$E2E_DIR" ]; then
          echo "Checking E2E test scripts in $E2E_DIR..."
          for script in "$E2E_DIR"/*.sh; do
            if [ -f "$script" ]; then
              echo "Checking syntax: $(basename "$script")"
              bash -n "$script"
            fi
          done

          echo "Running shellcheck on E2E scripts..."
          find "$E2E_DIR" -name '*.sh' -exec shellcheck -x --exclude=SC1091,SC2034 {} +
        fi

        echo "All test scripts pass syntax checks"
        mkdir $out
      '';
}
