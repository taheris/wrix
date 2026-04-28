# Test that README documentation matches actual flake exports
# Catches drift between docs and implementation (like lib vs legacyPackages.lib)
{
  pkgs,
  src,
}:

let
  # Extract the nix code block from README
  extractExample = pkgs.runCommandLocal "extract-readme-example" { } ''
    # Extract content between ```nix and ```
    ${pkgs.gnused}/bin/sed -n '/^```nix$/,/^```$/{ /^```/d; p; }' ${src}/README.md > $out
  '';

in
pkgs.runCommandLocal "test-readme-example" { } ''
  echo "Verifying README example structure..."

  # Verify example was extracted
  if [ ! -s ${extractExample} ]; then
    echo "ERROR: Could not extract nix example from README.md"
    exit 1
  fi

  echo "  Example extracted: OK"

  # Verify it contains the documented access pattern
  if ! grep -q 'legacyPackages.*\.lib' ${extractExample}; then
    echo "ERROR: README should document legacyPackages.\''${system}.lib"
    cat ${extractExample}
    exit 1
  fi

  echo "  Access pattern correct: OK"

  # Verify the flake-parts lib module exports legacyPackages.lib
  if ! grep -q 'legacyPackages\.lib' ${src}/modules/flake/lib.nix; then
    echo "ERROR: modules/flake/lib.nix should export legacyPackages.lib"
    exit 1
  fi

  echo "  Flake exports match: OK"

  echo "README example structure verified"
  mkdir $out
''
