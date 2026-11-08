{ pkgs }:
let
  inherit (pkgs) openssh;
  runner = import ../../lib/prek/runner.nix { inherit pkgs; };
in
pkgs.runCommandLocal "packaged-git-runtime-conformance"
  {
    nativeBuildInputs = [
      pkgs.coreutils
      openssh
    ];
  }
  ''
    set -euo pipefail
    export HOME="$TMPDIR/home" GIT_CONFIG_NOSYSTEM=1
    export GIT_CONFIG_GLOBAL="$TMPDIR/git-config"
    export GIT_TEMPLATE_DIR="$TMPDIR/empty-template"
    mkdir -p "$HOME" "$GIT_TEMPLATE_DIR"
    export PATH="$(${runner}/bin/wrix-prek --print-bin-dir):$PATH"
    ssh-keygen -q -t ed25519 -N "" -f "$TMPDIR/signing-key"
    printf 'fixture@example.invalid %s\n' "$(cat "$TMPDIR/signing-key.pub")" > "$TMPDIR/allowed-signers"
    git config --global user.name Fixture
    git config --global user.email fixture@example.invalid
    git config --global gpg.format ssh
    git config --global user.signingkey "$TMPDIR/signing-key"
    git config --global gpg.ssh.program ${openssh}/bin/ssh-keygen
    git config --global gpg.ssh.allowedSignersFile "$TMPDIR/allowed-signers"
    git init -q -b main source
    cd source
    printf 'signed source\n' > tracked
    git add tracked
    git commit -q -S -m 'Signed runtime conformance'
    git verify-commit HEAD
    git tag -s -m 'Signed runtime tag' runtime-tag
    git verify-tag runtime-tag
    git init --bare -q "$TMPDIR/upstream.git"
    git remote add origin "$TMPDIR/upstream.git"
    git push -q -u origin main --tags
    git worktree add -q -b topic "$TMPDIR/worktree"
    test "$(git -C "$TMPDIR/worktree" rev-parse --git-common-dir)" = "$PWD/.git"
    test "$(git -C "$TMPDIR/worktree" show HEAD:tracked)" = 'signed source'
    git clone -q --branch main "file://$TMPDIR/upstream.git" "$TMPDIR/clone"
    git -C "$TMPDIR/clone" verify-commit HEAD
    git -C "$TMPDIR/clone" verify-tag runtime-tag
    test "$(git -C "$TMPDIR/clone" log -1 --format=%s)" = 'Signed runtime conformance'
    test "$(git -C "$TMPDIR/clone" status --porcelain)" = ""
    touch "$out"
  ''
