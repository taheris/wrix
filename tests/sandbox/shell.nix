# Shell utility tests - verify security properties of shell helper functions
{
  pkgs,
}:

let
  inherit (pkgs) runCommandLocal;

  shellUtils = import ../../lib/util/shell.nix { };
  inherit (shellUtils) expandPathFn pruneStaleImages;

in
{
  # Security test: expand_path only expands safe variables
  # This is a positive security finding (wx-560) - the function prevents
  # command injection by only expanding ~, $HOME, and $USER.
  expand-path-safe = runCommandLocal "test-expand-path-safe" { } ''
    set -euo pipefail

    ${expandPathFn}

    # Set up known environment
    export HOME="/home/testuser"
    export USER="testuser"

    echo "Testing safe expansions..."

    # Test 1: Tilde expands to $HOME
    result=$(expand_path "~/foo")
    expected="/home/testuser/foo"
    [ "$result" = "$expected" ] || { echo "FAIL: ~ expansion: got '$result', expected '$expected'"; exit 1; }
    echo "PASS: ~ expands to \$HOME"

    # Test 2: $HOME expands
    result=$(expand_path '$HOME/bar')
    expected="/home/testuser/bar"
    [ "$result" = "$expected" ] || { echo "FAIL: \$HOME expansion: got '$result', expected '$expected'"; exit 1; }
    echo "PASS: \$HOME expands correctly"

    # Test 3: $USER expands
    result=$(expand_path '/tmp/$USER/data')
    expected="/tmp/testuser/data"
    [ "$result" = "$expected" ] || { echo "FAIL: \$USER expansion: got '$result', expected '$expected'"; exit 1; }
    echo "PASS: \$USER expands correctly"

    # Test 4: Combined expansions
    result=$(expand_path '~/$USER/config')
    expected="/home/testuser/testuser/config"
    [ "$result" = "$expected" ] || { echo "FAIL: combined expansion: got '$result', expected '$expected'"; exit 1; }
    echo "PASS: Combined expansions work"

    mkdir $out
  '';

  # Security test: expand_path does NOT execute arbitrary commands
  # Critical security property - malicious input should pass through unchanged
  expand-path-no-injection = runCommandLocal "test-expand-path-no-injection" { } ''
    set -euo pipefail

    ${expandPathFn}

    export HOME="/home/testuser"
    export USER="testuser"

    echo "Testing injection resistance..."

    # Test 1: $() command substitution is NOT executed
    result=$(expand_path '$(whoami)')
    expected='$(whoami)'
    [ "$result" = "$expected" ] || { echo "FAIL: \$() was expanded: got '$result', expected literal"; exit 1; }
    echo "PASS: \$(command) not executed"

    # Test 2: Backticks are NOT executed
    result=$(expand_path '`id`')
    expected='`id`'
    [ "$result" = "$expected" ] || { echo "FAIL: backticks were expanded: got '$result', expected literal"; exit 1; }
    echo "PASS: \`command\` not executed"

    # Test 3: Other environment variables are NOT expanded
    export MALICIOUS="/etc/passwd"
    result=$(expand_path '$MALICIOUS')
    expected='$MALICIOUS'
    [ "$result" = "$expected" ] || { echo "FAIL: \$MALICIOUS was expanded: got '$result', expected literal"; exit 1; }
    echo "PASS: \$MALICIOUS not expanded"

    # Test 4: $PATH is NOT expanded (common variable)
    result=$(expand_path '$PATH/bin')
    expected='$PATH/bin'
    [ "$result" = "$expected" ] || { echo "FAIL: \$PATH was expanded: got '$result', expected literal"; exit 1; }
    echo "PASS: \$PATH not expanded"

    # Test 5: Nested command in path
    result=$(expand_path '/tmp/$(rm -rf /)/file')
    expected='/tmp/$(rm -rf /)/file'
    [ "$result" = "$expected" ] || { echo "FAIL: nested command was expanded: got '$result'"; exit 1; }
    echo "PASS: Nested \$(rm -rf /) not executed"

    # Test 6: Arithmetic expansion is NOT evaluated
    result=$(expand_path '$((1+1))')
    expected='$((1+1))'
    [ "$result" = "$expected" ] || { echo "FAIL: arithmetic was evaluated: got '$result', expected literal"; exit 1; }
    echo "PASS: \$((arithmetic)) not evaluated"

    # Test 7: Brace expansion is NOT performed
    result=$(expand_path '{a,b,c}')
    expected='{a,b,c}'
    [ "$result" = "$expected" ] || { echo "FAIL: braces were expanded: got '$result', expected literal"; exit 1; }
    echo "PASS: {brace,expansion} not performed"

    mkdir $out
  '';

  prune-stale-images-empty-holder = runCommandLocal "test-prune-stale-images-empty-holder" { } ''
    set -euo pipefail

    cat > podman <<'EOF'
    #!/usr/bin/env bash
    set -euo pipefail

    case "$1" in
      images)
        printf '%s\n' \
          'localhost/wrapix-rust latest new-id' \
          'localhost/wrapix-rust c07457c7 old-id'
        ;;
      rmi)
        echo 'Error: image is in use by a container' >&2
        exit 2
        ;;
      ps)
        ;;
      inspect)
        if [[ "''${*: -1}" == "" ]]; then
          echo 'Error: name or ID cannot be empty' >&2
          exit 125
        fi
        ;;
      *)
        echo "unexpected podman command: $*" >&2
        exit 64
        ;;
    esac
    EOF
    chmod +x podman

    if ! {
      ${pruneStaleImages { cmd = "$PWD/podman"; }}
    } 2>stderr; then
      echo "FAIL: pruneStaleImages returned non-zero" >&2
      cat stderr >&2
      exit 1
    fi

    if grep -q 'name or ID cannot be empty' stderr; then
      echo "FAIL: holder workspace lookup ran with an empty holder" >&2
      cat stderr >&2
      exit 1
    fi

    if ! grep -q 'pinned by a container' stderr; then
      echo "FAIL: generic pinned-container notice missing" >&2
      cat stderr >&2
      exit 1
    fi

    mkdir "$out"
  '';

  # Edge cases and boundary conditions
  expand-path-edge-cases = runCommandLocal "test-expand-path-edge-cases" { } ''
    set -euo pipefail

    ${expandPathFn}

    export HOME="/home/testuser"
    export USER="testuser"

    echo "Testing edge cases..."

    # Test 1: Empty string
    result=$(expand_path "")
    expected=""
    [ "$result" = "$expected" ] || { echo "FAIL: empty string: got '$result'"; exit 1; }
    echo "PASS: Empty string handled"

    # Test 2: Just tilde
    result=$(expand_path "~")
    expected="/home/testuser"
    [ "$result" = "$expected" ] || { echo "FAIL: bare tilde: got '$result', expected '$expected'"; exit 1; }
    echo "PASS: Bare ~ works"

    # Test 3: Tilde in middle of path is NOT expanded (correct behavior)
    result=$(expand_path "/foo/~/bar")
    expected="/foo/~/bar"
    [ "$result" = "$expected" ] || { echo "FAIL: mid-path tilde: got '$result', expected '$expected'"; exit 1; }
    echo "PASS: Tilde in middle not expanded"

    # Test 4: Multiple $HOME occurrences
    result=$(expand_path '$HOME/$HOME')
    expected="/home/testuser//home/testuser"
    [ "$result" = "$expected" ] || { echo "FAIL: multiple \$HOME: got '$result', expected '$expected'"; exit 1; }
    echo "PASS: Multiple \$HOME expansions work"

    # Test 5: Absolute path unchanged
    result=$(expand_path "/absolute/path")
    expected="/absolute/path"
    [ "$result" = "$expected" ] || { echo "FAIL: absolute path changed: got '$result'"; exit 1; }
    echo "PASS: Absolute paths unchanged"

    mkdir $out
  '';
}
