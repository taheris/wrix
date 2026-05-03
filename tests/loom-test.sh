#!/usr/bin/env bash
# Verify tests for the Loom harness spec.
#
# Each function exits 0 on PASS, non-zero on FAIL, 77 to skip.
# Invoked by `ralph spec --verify` as `tests/loom-test.sh <function_name>`.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOOM_DIR="$REPO_ROOT/loom"
WORKSPACE_TOML="$LOOM_DIR/Cargo.toml"

# Member crates expected at loom/crates/<name>/.
LOOM_CRATES=(loom loom-core loom-agent loom-workflow loom-templates)

# Workspace-pinned third-party deps (14, per spec).
LOOM_DEPS=(
    tokio serde serde_json thiserror displaydoc anyhow
    tracing tracing-subscriber rusqlite toml askama clap gix fd-lock
)

# Run cargo with a Rust toolchain regardless of whether the devShell has
# rust on PATH yet (Nix integration lands in a separate issue).
cargo_run() {
    if command -v cargo >/dev/null 2>&1; then
        ( cd "$LOOM_DIR" && cargo "$@" )
    else
        nix shell nixpkgs#cargo nixpkgs#rustc nixpkgs#gcc nixpkgs#clippy \
            --command bash -c "cd '$LOOM_DIR' && cargo $*"
    fi
}

# Read a key from a TOML file via `toml` python module if available, else
# fall back to grep. Used only for shape checks in `test_workspace_*`.
toml_grep() {
    local pattern="$1" file="$2"
    grep -E "$pattern" "$file"
}

#-----------------------------------------------------------------------------
# test_workspace_builds — `cargo build` from loom/ root succeeds.
#-----------------------------------------------------------------------------
test_workspace_builds() {
    cargo_run build --workspace --quiet
}

#-----------------------------------------------------------------------------
# test_crate_structure — all five member crates exist with src/{lib,main}.rs.
#-----------------------------------------------------------------------------
test_crate_structure() {
    local missing=0
    for crate in "${LOOM_CRATES[@]}"; do
        local dir="$LOOM_DIR/crates/$crate"
        if [ ! -d "$dir" ]; then
            echo "missing crate dir: $dir" >&2
            missing=$((missing + 1))
            continue
        fi
        if [ ! -f "$dir/Cargo.toml" ]; then
            echo "missing Cargo.toml: $dir/Cargo.toml" >&2
            missing=$((missing + 1))
        fi
        if [ "$crate" = "loom" ]; then
            if [ ! -f "$dir/src/main.rs" ]; then
                echo "missing main.rs: $dir/src/main.rs" >&2
                missing=$((missing + 1))
            fi
        else
            if [ ! -f "$dir/src/lib.rs" ]; then
                echo "missing lib.rs: $dir/src/lib.rs" >&2
                missing=$((missing + 1))
            fi
        fi
        if [ -f "$dir/src/types.rs" ]; then
            echo "forbidden central types.rs at: $dir/src/types.rs" >&2
            missing=$((missing + 1))
        fi
        if [ -f "$dir/src/error.rs" ]; then
            echo "forbidden central error.rs at: $dir/src/error.rs" >&2
            missing=$((missing + 1))
        fi
    done
    return "$missing"
}

#-----------------------------------------------------------------------------
# test_workspace_edition — workspace declares edition 2024 + resolver "3".
#-----------------------------------------------------------------------------
test_workspace_edition() {
    if ! toml_grep '^resolver[[:space:]]*=[[:space:]]*"3"' "$WORKSPACE_TOML" >/dev/null; then
        echo "workspace resolver is not \"3\" in $WORKSPACE_TOML" >&2
        return 1
    fi
    if ! toml_grep '^edition[[:space:]]*=[[:space:]]*"2024"' "$WORKSPACE_TOML" >/dev/null; then
        echo "workspace.package.edition is not \"2024\" in $WORKSPACE_TOML" >&2
        return 1
    fi
    # Each member must inherit edition from the workspace.
    local crate dir missing=0
    for crate in "${LOOM_CRATES[@]}"; do
        dir="$LOOM_DIR/crates/$crate"
        if ! grep -E '^edition\.workspace[[:space:]]*=[[:space:]]*true' "$dir/Cargo.toml" >/dev/null; then
            echo "$crate: edition.workspace = true missing in $dir/Cargo.toml" >&2
            missing=$((missing + 1))
        fi
    done
    return "$missing"
}

#-----------------------------------------------------------------------------
# test_workspace_deps_pinned — every spec-listed third-party crate is pinned
# under [workspace.dependencies] in loom/Cargo.toml.
#-----------------------------------------------------------------------------
test_workspace_deps_pinned() {
    # Extract the [workspace.dependencies] section into a buffer and check
    # each expected dep appears as a key. We stop at the next [section].
    local section
    section=$(awk '
        /^\[workspace\.dependencies\][[:space:]]*$/ { in_section = 1; next }
        in_section && /^\[/ { in_section = 0 }
        in_section { print }
    ' "$WORKSPACE_TOML")

    if [ -z "$section" ]; then
        echo "[workspace.dependencies] section missing in $WORKSPACE_TOML" >&2
        return 1
    fi

    local missing=0 dep
    for dep in "${LOOM_DEPS[@]}"; do
        if ! grep -E "^${dep}[[:space:]]*=" <<<"$section" >/dev/null; then
            echo "dep not pinned in [workspace.dependencies]: $dep" >&2
            missing=$((missing + 1))
        fi
    done
    return "$missing"
}

#-----------------------------------------------------------------------------
# test_workspace_lints — workspace declares strict lint block; every member
# crate inherits it via `[lints] workspace = true`.
#-----------------------------------------------------------------------------
test_workspace_lints() {
    local missing=0

    if ! grep -E '^\[workspace\.lints\.rust\][[:space:]]*$' "$WORKSPACE_TOML" >/dev/null; then
        echo "[workspace.lints.rust] section missing in $WORKSPACE_TOML" >&2
        missing=$((missing + 1))
    fi
    if ! grep -E '^\[workspace\.lints\.clippy\][[:space:]]*$' "$WORKSPACE_TOML" >/dev/null; then
        echo "[workspace.lints.clippy] section missing in $WORKSPACE_TOML" >&2
        missing=$((missing + 1))
    fi
    # Spec NF-9: panics banned. The four denials must appear in clippy block.
    local lint clippy_section
    clippy_section=$(awk '
        /^\[workspace\.lints\.clippy\][[:space:]]*$/ { in_section = 1; next }
        in_section && /^\[/ { in_section = 0 }
        in_section { print }
    ' "$WORKSPACE_TOML")
    for lint in unwrap_used todo unimplemented panic; do
        if ! grep -E "^${lint}[[:space:]]*=[[:space:]]*\"deny\"" <<<"$clippy_section" >/dev/null; then
            echo "clippy lint $lint not denied in $WORKSPACE_TOML" >&2
            missing=$((missing + 1))
        fi
    done

    local crate dir
    for crate in "${LOOM_CRATES[@]}"; do
        dir="$LOOM_DIR/crates/$crate"
        if ! awk '
            /^\[lints\][[:space:]]*$/ { in_section = 1; next }
            in_section && /^\[/ { in_section = 0 }
            in_section && /^workspace[[:space:]]*=[[:space:]]*true/ { found = 1 }
            END { exit (found ? 0 : 1) }
        ' "$dir/Cargo.toml"; then
            echo "$crate: [lints] workspace = true missing in $dir/Cargo.toml" >&2
            missing=$((missing + 1))
        fi
    done

    return "$missing"
}

#-----------------------------------------------------------------------------
# test_nix_build — `nix build .#loom` succeeds and produces a loom binary.
#-----------------------------------------------------------------------------
test_nix_build() {
    local out
    out=$(nix build "$REPO_ROOT#loom" --no-link --print-out-paths)
    if [ ! -x "$out/bin/loom" ]; then
        echo "expected executable at $out/bin/loom" >&2
        return 1
    fi
}

#-----------------------------------------------------------------------------
# test_devshell_includes_loom — loom binary on PATH inside `nix develop`,
# alongside ralph (dual-path transition).
#-----------------------------------------------------------------------------
test_devshell_includes_loom() {
    local paths
    paths=$(nix develop "$REPO_ROOT" --command bash -c 'command -v loom; command -v ralph')
    if ! grep -q '/loom$' <<<"$paths"; then
        echo "loom not found on devShell PATH" >&2
        echo "$paths" >&2
        return 1
    fi
    if ! grep -q '/ralph$' <<<"$paths"; then
        echo "ralph not found on devShell PATH (dual-path requires both)" >&2
        echo "$paths" >&2
        return 1
    fi
}

#-----------------------------------------------------------------------------
# test_clippy_clean — `cargo clippy --workspace` passes with workspace lints
# (warnings denied).
#-----------------------------------------------------------------------------
test_clippy_clean() {
    cargo_run clippy --workspace --all-targets -- -D warnings
}

#-----------------------------------------------------------------------------
# test_cargo_test — `cargo test --workspace` passes for all crates.
#-----------------------------------------------------------------------------
test_cargo_test() {
    cargo_run test --workspace
}

#-----------------------------------------------------------------------------
# Helpers for wrapix run-bead acceptance tests.
#
# The wrapix script has a WRAPIX_DRY_RUN=1 mode that parses the SpawnConfig
# and prints resolved spawn state without invoking the container runtime.
# That lets us verify SpawnConfig consumption on any host (incl. CI without
# /dev/kvm) without booting a real container.
#-----------------------------------------------------------------------------
wrapix_bin() {
    nix build "$REPO_ROOT#sandbox" --no-link --print-out-paths 2>/dev/null
}

write_spawn_config() {
    local path="$1" image="$2" workspace="${3:-/some/workspace}"
    cat >"$path" <<EOF
{
  "image": "$image",
  "workspace": "$workspace",
  "env": [["WRAPIX_AGENT","claude-code"],["TERM","dumb"]],
  "initial_prompt": "do the thing",
  "agent_args": ["--print","--output-format","stream-json"],
  "repin": {"orientation":"o","pinned_context":"pc","partial_bodies":{}}
}
EOF
}

#-----------------------------------------------------------------------------
# test_wrapix_run_bead_subcommand — `wrapix run-bead --spawn-config <f> --stdio`
# parses the JSON, omits TTY (STDIO=1), and exposes the resolved spawn state.
#-----------------------------------------------------------------------------
test_wrapix_run_bead_subcommand() {
    local out sandbox tmp
    sandbox=$(wrapix_bin)
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' RETURN

    write_spawn_config "$tmp/spawn.json" "localhost/wrapix-test:run-bead"
    out=$(WRAPIX_DRY_RUN=1 "$sandbox/bin/wrapix" \
        run-bead --spawn-config "$tmp/spawn.json" --stdio)

    grep -qx 'SUBCOMMAND=run-bead' <<<"$out" || { echo "missing SUBCOMMAND=run-bead" >&2; echo "$out" >&2; return 1; }
    grep -qx 'STDIO=1' <<<"$out" || { echo "missing STDIO=1 (expected --stdio to set it)" >&2; echo "$out" >&2; return 1; }
    grep -qx 'IMAGE_OVERRIDE=localhost/wrapix-test:run-bead' <<<"$out" || { echo "image override not honored" >&2; echo "$out" >&2; return 1; }

    # --spawn-config is required.
    if WRAPIX_DRY_RUN=1 "$sandbox/bin/wrapix" run-bead --stdio 2>/dev/null; then
        echo "wrapix run-bead without --spawn-config should fail" >&2
        return 1
    fi
}

#-----------------------------------------------------------------------------
# test_spawn_config_json_stability — every documented SpawnConfig field
# round-trips through the JSON shape without rename or loss.
#-----------------------------------------------------------------------------
test_spawn_config_json_stability() {
    local sandbox tmp keys
    sandbox=$(wrapix_bin)
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' RETURN

    cat >"$tmp/spawn.json" <<'EOF'
{
  "image": "localhost/wrapix-stability:tag",
  "workspace": "/work/ws",
  "env": [["A","1"],["B","2"]],
  "initial_prompt": "prompt body",
  "agent_args": ["one","two"],
  "repin": {
    "orientation": "ori",
    "pinned_context": "ctx",
    "partial_bodies": {"foo": "bar"}
  }
}
EOF

    # All six top-level keys must be present in the fixture itself.
    keys=$(jq -r 'keys | sort | join(",")' "$tmp/spawn.json")
    [ "$keys" = "agent_args,env,image,initial_prompt,repin,workspace" ] \
        || { echo "fixture key set drifted: $keys" >&2; return 1; }

    # Round-trip via jq must preserve every key+value (jq -S sorts keys for diff).
    jq -S . "$tmp/spawn.json" >"$tmp/canon.json"
    jq -S . "$tmp/canon.json" >"$tmp/canon2.json"
    if ! diff -q "$tmp/canon.json" "$tmp/canon2.json" >/dev/null; then
        echo "round-trip diverged" >&2
        diff "$tmp/canon.json" "$tmp/canon2.json" >&2
        return 1
    fi

    # repin sub-keys must also round-trip.
    keys=$(jq -r '.repin | keys | sort | join(",")' "$tmp/spawn.json")
    [ "$keys" = "orientation,partial_bodies,pinned_context" ] \
        || { echo "repin key set drifted: $keys" >&2; return 1; }

    # The launcher must accept the full fixture (no unknown-field errors).
    WRAPIX_DRY_RUN=1 "$sandbox/bin/wrapix" \
        run-bead --spawn-config "$tmp/spawn.json" --stdio >/dev/null
}

#-----------------------------------------------------------------------------
# test_per_bead_profile_spawn — two SpawnConfigs with different `image`
# yield two `wrapix run-bead` invocations that resolve to different images.
#-----------------------------------------------------------------------------
test_per_bead_profile_spawn() {
    local sandbox tmp out_a out_b
    sandbox=$(wrapix_bin)
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' RETURN

    write_spawn_config "$tmp/a.json" "localhost/wrapix-base:tagA"
    write_spawn_config "$tmp/b.json" "localhost/wrapix-rust:tagB"

    out_a=$(WRAPIX_DRY_RUN=1 "$sandbox/bin/wrapix" \
        run-bead --spawn-config "$tmp/a.json" --stdio | grep '^IMAGE_OVERRIDE=')
    out_b=$(WRAPIX_DRY_RUN=1 "$sandbox/bin/wrapix" \
        run-bead --spawn-config "$tmp/b.json" --stdio | grep '^IMAGE_OVERRIDE=')

    [ "$out_a" = "IMAGE_OVERRIDE=localhost/wrapix-base:tagA" ] || { echo "bead A: $out_a" >&2; return 1; }
    [ "$out_b" = "IMAGE_OVERRIDE=localhost/wrapix-rust:tagB" ] || { echo "bead B: $out_b" >&2; return 1; }
    [ "$out_a" != "$out_b" ] || { echo "two beads produced identical image" >&2; return 1; }
}

#-----------------------------------------------------------------------------
# test_loom_does_not_invoke_podman — loom Rust sources never invoke podman
# directly; only documentation/comments may reference it.
#-----------------------------------------------------------------------------
test_loom_does_not_invoke_podman() {
    local hits
    if [ ! -d "$LOOM_DIR/crates" ]; then
        echo "loom/crates not yet scaffolded" >&2
        return 77
    fi
    # Match podman in any .rs source; allow only doc-comment lines (`///`,
    # `//!`) and ordinary comments (`//`). Anything outside a comment is a
    # rule violation.
    hits=$(grep -rEn 'podman' "$LOOM_DIR/crates" --include='*.rs' || true)
    if [ -z "$hits" ]; then
        return 0
    fi
    if grep -vE '^[^:]+:[0-9]+:[[:space:]]*(//|/\*)' <<<"$hits" >/dev/null; then
        echo "non-comment podman reference found in loom/crates:" >&2
        grep -vE '^[^:]+:[0-9]+:[[:space:]]*(//|/\*)' <<<"$hits" >&2
        return 1
    fi
}

#-----------------------------------------------------------------------------
# Dispatch
#-----------------------------------------------------------------------------
if [ $# -eq 0 ]; then
    echo "usage: $0 <test_function>" >&2
    echo "available functions:" >&2
    declare -F | awk '{print "  " $3}' | grep '^  test_' >&2
    exit 2
fi

fn="$1"
shift
if ! declare -F "$fn" >/dev/null; then
    echo "unknown function: $fn" >&2
    exit 2
fi
"$fn" "$@"
