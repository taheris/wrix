# Shell utility tests - verify security properties of shell helper functions
{
  pkgs,
}:

let
  inherit (pkgs) runCommandLocal;

  shellUtils = import ../../lib/util/shell.nix { inherit pkgs; };
  inherit (shellUtils) expandPathFn pruneStaleImages rememberImageRef;

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

  known-hosts-installer-repairs-unwritable-ssh-dir =
    if !pkgs.stdenv.hostPlatform.isLinux then
      runCommandLocal "test-known-hosts-installer-skipped" { } ''
        set -euo pipefail
        echo "SKIP: known_hosts installer permission test is Linux-only" >&2
        mkdir "$out"
      ''
    else
      runCommandLocal "test-known-hosts-installer-repairs-unwritable-ssh-dir"
        {
          nativeBuildInputs = [ pkgs.util-linux ];
        }
        ''
          set -euo pipefail

          chmod 0755 "$PWD"
          root="$PWD/root"
          known_hosts="$PWD/known_hosts"
          mkdir -p "$root/etc/ssh"
          printf '%s\n' 'github.com ssh-ed25519 test' > "$known_hosts"
          chmod 0555 "$root/etc/ssh"

          runner=()
          if [[ "$(id -u)" = "0" ]]; then
            chown -R 65534:65534 "$root" "$known_hosts"
            runner=(setpriv --reuid=65534 --regid=65534 --clear-groups)
          fi

          "''${runner[@]}" ${pkgs.bash}/bin/bash ${../../lib/sandbox/install-known-hosts.sh} "$known_hosts" "$root"

          cmp "$known_hosts" "$root/etc/ssh/ssh_known_hosts"
          cmp "$known_hosts" "$root/etc/wrix/known_hosts_dir/known_hosts"
          mode=$(stat -c %a "$root/etc/ssh")
          if [[ "$mode" != "755" ]]; then
            echo "FAIL: /etc/ssh mode is $mode, expected 755" >&2
            exit 1
          fi

          mkdir "$out"
        '';

  prune-stale-images-empty-holder = runCommandLocal "test-prune-stale-images-empty-holder" { } ''
    set -euo pipefail

    cat > podman <<'EOF'
    #!${pkgs.bash}/bin/bash
    set -euo pipefail

    case "$1" in
      images)
        printf '%s\n' \
          'localhost/wrix-rust latest new-id' \
          'localhost/wrix-rust c07457c7 old-id'
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

  remember-image-mru-bounds-and-identifiers =
    runCommandLocal "test-remember-image-mru-bounds-and-identifiers" { }
      ''
        set -euo pipefail

        export WRIX_CACHE="$PWD/cache"
        mkdir -p "$WRIX_CACHE" "$PWD/bin"
        cat > "$PWD/bin/podman" <<'EOF'
        #!${pkgs.bash}/bin/bash
        set -euo pipefail
        if [[ "$1 $2" == "image inspect" && "$3" == "--format" ]]; then
          ref="''${5##*:}"
          printf 'id-%s\n' "$ref"
          exit 0
        fi
        echo "unexpected podman command: $*" >&2
        exit 64
        EOF
        chmod +x "$PWD/bin/podman"
        export PATH="$PWD/bin:$PATH"

        for idx in $(seq 1 10); do
          IMAGE_REF="localhost/wrix-rust:tag$idx"
          IMAGE_DIGEST_PATH="sha256:00000000000000000000000000000000000000000000000000000000000000$idx"
          ${rememberImageRef}
        done

        count=$(${pkgs.jq}/bin/jq 'length' "$WRIX_CACHE/image-mru.json")
        if [[ "$count" != "8" ]]; then
          echo "FAIL: MRU length is $count, expected 8" >&2
          cat "$WRIX_CACHE/image-mru.json" >&2
          exit 1
        fi

        first_ref=$(${pkgs.jq}/bin/jq -r '.[0].ref' "$WRIX_CACHE/image-mru.json")
        last_ref=$(${pkgs.jq}/bin/jq -r '.[7].ref' "$WRIX_CACHE/image-mru.json")
        first_id=$(${pkgs.jq}/bin/jq -r '.[0].id' "$WRIX_CACHE/image-mru.json")
        first_digest=$(${pkgs.jq}/bin/jq -r '.[0].digest' "$WRIX_CACHE/image-mru.json")
        if [[ "$first_ref" != "localhost/wrix-rust:tag10" || "$last_ref" != "localhost/wrix-rust:tag3" ]]; then
          echo "FAIL: MRU order/bound mismatch" >&2
          cat "$WRIX_CACHE/image-mru.json" >&2
          exit 1
        fi
        if [[ "$first_id" != "id-tag10" || "$first_digest" != sha256:*0010 ]]; then
          echo "FAIL: MRU did not record image id and digest" >&2
          cat "$WRIX_CACHE/image-mru.json" >&2
          exit 1
        fi

        mkdir "$out"
      '';

  prune-stale-images-keeps-recorded-ref =
    runCommandLocal "test-prune-stale-images-keeps-recorded-ref" { }
      ''
        set -euo pipefail

        export WRIX_CACHE="$PWD/cache"
        mkdir -p "$WRIX_CACHE"
        cat > "$WRIX_CACHE/image-mru.json" <<'JSON'
        [{"ref":"localhost/wrix-rust:c07457c7"}]
        JSON

        cat > podman <<'EOF'
        #!${pkgs.bash}/bin/bash
        set -euo pipefail

        case "$1" in
          images)
            printf '%s\n' \
              'localhost/wrix-rust latest new-id' \
              'localhost/wrix-rust c07457c7 old-id' \
              'localhost/wrix-rust abandoned older-id'
            ;;
          image)
            exit 1
            ;;
          ps)
            ;;
          rmi)
            printf '%s\n' "$*" >> rmi.log
            ;;
          *)
            echo "unexpected podman command: $*" >&2
            exit 64
            ;;
        esac
        EOF
        chmod +x podman
        : > rmi.log

        ${pruneStaleImages { cmd = "$PWD/podman"; }}

        if grep -q 'localhost/wrix-rust:c07457c7' rmi.log; then
          echo "FAIL: recorded ref was pruned" >&2
          cat rmi.log >&2
          exit 1
        fi

        if ! grep -q 'localhost/wrix-rust:abandoned' rmi.log; then
          echo "FAIL: unrecorded stale ref was not pruned" >&2
          cat rmi.log >&2
          exit 1
        fi

        mkdir "$out"
      '';

  prune-stale-images-retention-policy =
    runCommandLocal "test-prune-stale-images-retention-policy" { }
      ''
        set -euo pipefail

        export WRIX_CACHE="$PWD/cache"
        mkdir -p "$WRIX_CACHE"
        IMAGE_REF="localhost/wrix-current:live"
        IMAGE_DIGEST_PATH="sha256:current-digest"
        cat > "$WRIX_CACHE/image-mru.json" <<'JSON'
        [
          {"ref":"localhost/wrix-recent-by-ref:old"},
          {"digest":"sha256:recent-digest"},
          {"id":"recent-id"}
        ]
        JSON

        cat > podman <<'EOF'
        #!${pkgs.bash}/bin/bash
        set -euo pipefail

        image_id() {
          case "$1" in
            localhost/wrix-current:live) printf 'current-id\n' ;;
            localhost/wrix-recent-by-digest:old) printf 'recent-digest-id\n' ;;
            localhost/wrix-recent-by-id:old) printf 'recent-id\n' ;;
            localhost/wrix-used:old) printf 'used-id\n' ;;
            localhost/wrix-stale:old) printf 'stale-id\n' ;;
            localhost/wrix-legacy:old) printf 'legacy-id\n' ;;
            managed-dangling-id) printf 'managed-dangling-id\n' ;;
            dangling-id) printf 'dangling-id\n' ;;
            *) printf '%s\n' "$1" ;;
          esac
        }

        image_digest() {
          case "$1" in
            localhost/wrix-current:live) printf 'sha256:current-digest\n' ;;
            localhost/wrix-recent-by-digest:old) printf 'sha256:recent-digest\n' ;;
            *) printf '<no value>\n' ;;
          esac
        }

        managed_label() {
          case "$1" in
            localhost/wrix-current:live|localhost/wrix-recent-by-ref:old|localhost/wrix-recent-by-digest:old|localhost/wrix-recent-by-id:old|localhost/wrix-used:old|localhost/wrix-stale:old|managed-dangling-id) printf 'true\n' ;;
            *) printf '<no value>\n' ;;
          esac
        }

        case "$1" in
          images)
            printf '%s\n' \
              'localhost/wrix-current live current-id' \
              'localhost/wrix-recent-by-ref old recent-ref-id' \
              'localhost/wrix-recent-by-digest old recent-digest-id' \
              'localhost/wrix-recent-by-id old recent-id' \
              'localhost/wrix-used old used-id' \
              'localhost/wrix-stale old stale-id' \
              'localhost/wrix-legacy old legacy-id' \
              '<none> <none> dangling-id' \
              '<none> <none> managed-dangling-id' \
              'docker.io/library/ubuntu latest user-id'
            ;;
          image)
            target="''${@: -1}"
            case "''${4:-}" in
              '{{.Id}}') image_id "$target" ;;
              '{{.Digest}}') image_digest "$target" ;;
              '{{ index .Config.Labels "wrix.managed" }}') managed_label "$target" ;;
              *) echo "unexpected image inspect format: $*" >&2; exit 64 ;;
            esac
            ;;
          ps)
            if [[ "$*" == *'ancestor=localhost/wrix-used:old'* ]]; then
              printf 'service-holder\n'
            fi
            ;;
          rmi)
            printf '%s\n' "$2" >> rmi.log
            ;;
          *)
            echo "unexpected podman command: $*" >&2
            exit 64
            ;;
        esac
        EOF
        chmod +x podman
        : > rmi.log

        ${pruneStaleImages { cmd = "$PWD/podman"; }}

        for kept in \
          'localhost/wrix-current:live' \
          'localhost/wrix-recent-by-ref:old' \
          'localhost/wrix-recent-by-digest:old' \
          'localhost/wrix-recent-by-id:old' \
          'localhost/wrix-used:old' \
          'dangling-id' \
          'docker.io/library/ubuntu:latest'; do
          if grep -Fxq "$kept" rmi.log; then
            echo "FAIL: kept image was pruned: $kept" >&2
            cat rmi.log >&2
            exit 1
          fi
        done

        for pruned in \
          'localhost/wrix-stale:old' \
          'localhost/wrix-legacy:old' \
          'managed-dangling-id'; do
          if ! grep -Fxq "$pruned" rmi.log; then
            echo "FAIL: stale image was not pruned: $pruned" >&2
            cat rmi.log >&2
            exit 1
          fi
        done

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
