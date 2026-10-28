{ pkgs, wrix }:

pkgs.runCommand "shared-nix-host-binding"
  {
    nativeBuildInputs = [
      pkgs.git
      pkgs.gnugrep
    ];
  }
  ''
    set -euo pipefail
    export HOME="$TMPDIR/home"
    export XDG_STATE_HOME="$TMPDIR/state"
    export XDG_CACHE_HOME="$TMPDIR/cache"
    mkdir -p "$HOME" "$TMPDIR/project-one" "$TMPDIR/project-two"
    git -c init.defaultBranch=main init -q "$TMPDIR/project-one"
    git -c init.defaultBranch=main init -q "$TMPDIR/project-two"
    printf '{"image":"project-one"}\n' > "$TMPDIR/project-one.json"
    printf '{"image":"project-two"}\n' > "$TMPDIR/project-two.json"
    cd "$TMPDIR/project-one"
    WRIX_PROFILE_CONFIG="$TMPDIR/project-one.json" \
      ${wrix.rustPackage.wrix}/bin/wrix service nix-store > "$TMPDIR/one"
    cd "$TMPDIR/project-two"
    WRIX_PROFILE_CONFIG="$TMPDIR/project-two.json" \
      ${wrix.rustPackage.wrix}/bin/wrix service nix-store > "$TMPDIR/two"
    for report in "$TMPDIR/one" "$TMPDIR/two"; do
      grep -Fx 'pool service image: ${wrix.nixPoolImage.ref} (package-selected candidate)' "$report"
      grep -Fx 'installed-store worker delivery: launch-specific; inspect runtime, image and approval with wrix run --dry-run --json' "$report"
    done
    if WRIX_NIX_POOL_IMAGE=untrusted-project-override \
      ${wrix.rustPackage.wrix}/bin/wrix service nix-store > "$TMPDIR/rejected" 2>&1; then
      printf 'Project image alias replaced the dedicated package binding.\n' >&2
      exit 1
    fi
    grep -F 'WRIX_NIX_POOL_IMAGE cannot select Nix pool software' "$TMPDIR/rejected"
    [[ ! -e "$XDG_STATE_HOME/wrix" ]]
    [[ ! -e "$XDG_CACHE_HOME/wrix" ]]
    touch "$out"
  ''
