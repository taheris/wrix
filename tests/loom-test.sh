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
# test_no_panics_in_production — non-test Rust code under loom/crates/ contains
# no `unwrap()`, `expect(...)`, `todo!()`, `unimplemented!()`, or `panic!()`
# calls. Test code (`#[cfg(test)]` modules and files under `tests/`) is
# excluded; the workspace clippy block already denies these in production.
#-----------------------------------------------------------------------------
test_no_panics_in_production() {
    if [ ! -d "$LOOM_DIR/crates" ]; then
        echo "loom/crates not yet scaffolded" >&2
        return 77
    fi
    local hits violations=0
    while IFS= read -r -d '' file; do
        # Skip integration test directories entirely.
        case "$file" in
            */tests/*) continue ;;
        esac
        # Strip `#[cfg(test)] mod tests { ... }` blocks before scanning so
        # unit tests under the same source file are excluded.
        local body
        body=$(awk '
            BEGIN { skip = 0; depth = 0 }
            /^[[:space:]]*#\[cfg\(test\)\][[:space:]]*$/ { skip = 1; next }
            skip && /\{/ { depth += gsub(/\{/, "{") }
            skip && /\}/ {
                depth -= gsub(/\}/, "}")
                if (depth <= 0) { skip = 0; depth = 0; next }
            }
            !skip { print }
        ' "$file")
        hits=$(grep -nE '\b(unwrap|expect|todo|unimplemented|panic)\s*\(' <<<"$body" || true)
        # Filter out comment lines and the macro pattern in identifier/mod.rs
        # where `unwrap`/`panic` could appear in doc strings.
        hits=$(grep -vE '^[[:space:]]*[0-9]+:[[:space:]]*(//|/\*|\*)' <<<"$hits" || true)
        if [ -n "$hits" ]; then
            echo "panic-in-production candidate(s) in $file:" >&2
            echo "$hits" >&2
            violations=$((violations + 1))
        fi
    done < <(find "$LOOM_DIR/crates" -name '*.rs' -print0)
    return "$violations"
}

#-----------------------------------------------------------------------------
# test_no_allow_dead_code — non-test Rust code uses `#[expect(dead_code)]`,
# never `#[allow(dead_code)]` (NF-9). `expect` fails the build if the warning
# stops firing; `allow` silently rots.
#-----------------------------------------------------------------------------
test_no_allow_dead_code() {
    if [ ! -d "$LOOM_DIR/crates" ]; then
        echo "loom/crates not yet scaffolded" >&2
        return 77
    fi
    local hits
    hits=$(grep -rEn '#\[allow\(dead_code\)\]' "$LOOM_DIR/crates" --include='*.rs' || true)
    if [ -n "$hits" ]; then
        echo "forbidden #[allow(dead_code)] (use #[expect(dead_code)] instead):" >&2
        echo "$hits" >&2
        return 1
    fi
}

#-----------------------------------------------------------------------------
# test_no_derive_from_on_newtypes — the newtype_id! macro and any newtype
# struct must not derive `From` or `Into` (NF-8: bypasses the newtype
# boundary). `#[from]` is permitted only on error enum variants — guarded by
# scoping the search to identifier/ submodules where the newtypes live.
#-----------------------------------------------------------------------------
test_no_derive_from_on_newtypes() {
    local id_dir="$LOOM_DIR/crates/loom-core/src/identifier"
    if [ ! -d "$id_dir" ]; then
        echo "loom-core identifier module not yet scaffolded" >&2
        return 77
    fi
    local hits
    # Look at every #[derive(...)] line under identifier/ for a From or Into
    # bare ident. Match on word boundaries so `TryFrom`, `IntoIterator`, etc.
    # don't false-positive (none of those are valid in derive lists anyway).
    hits=$(grep -rEn '#\[derive\([^)]*\b(From|Into)\b[^)]*\)\]' "$id_dir" --include='*.rs' || true)
    if [ -n "$hits" ]; then
        echo "forbidden derive(From) or derive(Into) on newtype:" >&2
        echo "$hits" >&2
        return 1
    fi
    # Same for the macro definition itself: the body must not splice From/Into
    # into the derive list.
    hits=$(awk '
        /macro_rules![[:space:]]*newtype_id/ { in_macro = 1 }
        in_macro && /#\[derive\(/ { print NR ": " $0 }
        in_macro && /^[[:space:]]*\}[[:space:]]*$/ { in_macro = 0 }
    ' "$id_dir/mod.rs" | grep -E '\b(From|Into)\b' || true)
    if [ -n "$hits" ]; then
        echo "newtype_id! macro derives From/Into:" >&2
        echo "$hits" >&2
        return 1
    fi
}

#-----------------------------------------------------------------------------
# test_askama_templates_compile — `cargo build -p loom-templates` succeeds.
# Askama runs its template parser at compile time, so a successful build is
# proof every template parsed and every typed context covers its variables.
#-----------------------------------------------------------------------------
test_askama_templates_compile() {
    cargo_run build -p loom-templates --quiet
}

#-----------------------------------------------------------------------------
# test_template_compile_time_check — every template file has a typed context
# struct under loom-templates/src/, and the crate's render integration tests
# (which exercise every Template::render impl) pass. A missing context field
# fails askama's derive at compile time, so a green run here proves the
# compile-time check is enforced.
#-----------------------------------------------------------------------------
test_template_compile_time_check() {
    local templates_dir="$LOOM_DIR/crates/loom-templates/templates"
    local missing=0 t
    if [ ! -d "$templates_dir" ]; then
        echo "loom-templates/templates not yet scaffolded" >&2
        return 77
    fi
    for t in plan_new plan_update todo_new todo_update run check msg; do
        if [ ! -f "$templates_dir/$t.md" ]; then
            echo "missing template: $templates_dir/$t.md" >&2
            missing=$((missing + 1))
        fi
    done
    cargo_run test -p loom-templates --test render --quiet
    return "$missing"
}

#-----------------------------------------------------------------------------
# test_template_partials — every loom partial lives under templates/partial/
# and is referenced via askama's `{% include %}` (not the legacy `{{> name}}`
# syntax).
#-----------------------------------------------------------------------------
test_template_partials() {
    local partials_dir="$LOOM_DIR/crates/loom-templates/templates/partial"
    local templates_dir="$LOOM_DIR/crates/loom-templates/templates"
    local missing=0 p
    if [ ! -d "$partials_dir" ]; then
        echo "loom partials directory missing: $partials_dir" >&2
        return 1
    fi
    for p in context_pinning spec_header exit_signals companions_context; do
        if [ ! -f "$partials_dir/$p.md" ]; then
            echo "missing partial: $partials_dir/$p.md" >&2
            missing=$((missing + 1))
        fi
    done
    if grep -rE '\{\{>[[:space:]]+[a-z_-]+[[:space:]]*\}\}' "$templates_dir" >/dev/null; then
        echo "legacy {{> partial}} syntax found in loom templates (expected {% include %}):" >&2
        grep -rnE '\{\{>[[:space:]]+[a-z_-]+[[:space:]]*\}\}' "$templates_dir" >&2
        missing=$((missing + 1))
    fi
    if ! grep -rE '\{%[[:space:]]+include[[:space:]]+"partial/' "$templates_dir" >/dev/null; then
        echo "no askama include directives found in loom templates" >&2
        missing=$((missing + 1))
    fi
    return "$missing"
}

#-----------------------------------------------------------------------------
# test_template_output_parity — the loom-templates render integration test
# `run_output_parity_with_ralph_for_shared_inputs` produces output that
# preserves every section and substituted value Ralph's bash renderer emits
# for the same inputs (modulo intentional drift: dropped legacy variables and
# the `ralph` → `loom` driver rename).
#-----------------------------------------------------------------------------
test_template_output_parity() {
    cargo_run test -p loom-templates --test render \
        run_output_parity_with_ralph_for_shared_inputs --quiet
}

#-----------------------------------------------------------------------------
# Workflow commands — each function dispatches into the matching cargo unit
# test under `loom-workflow`. Sharing the cargo binary keeps verify and
# `cargo test` exercising the same code paths.
#-----------------------------------------------------------------------------
todo_cargo_test() {
    cargo_run test -p loom-workflow --lib "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_todo_tier_detection — `compute_spec_diff` correctly classifies inputs
# into the four tiers from `lib/ralph/cmd/util.sh::compute_spec_diff`:
#   - Tier 1 (diff): molecule + valid base_commit → per-spec fan-out, with
#     sibling overrides and orphan fallback to the anchor's base.
#   - Tier 2 (tasks): molecule present but base_commit absent / orphaned /
#     missing → fall back to existing-task comparison.
#   - Tier 4 (new): no molecule recorded → fresh decomposition.
#   - --since override: replaces the anchor's base for the anchor only;
#     errors when the override commit is missing or orphaned.
# (Tier 3 — README discovery — is the driver's responsibility before
# `compute_spec_diff` runs; once it reconstructs a molecule, the call
# proceeds as tier 2.)
#-----------------------------------------------------------------------------
test_todo_tier_detection() {
    todo_cargo_test todo::tier::tests::tier_4_when_no_molecule_no_since
    todo_cargo_test todo::tier::tests::tier_2_when_molecule_present_without_base_commit
    todo_cargo_test todo::tier::tests::tier_2_when_base_commit_orphaned
    todo_cargo_test todo::tier::tests::tier_2_when_base_commit_no_longer_exists
    todo_cargo_test todo::tier::tests::tier_1_diff_with_empty_candidate_set
    todo_cargo_test todo::tier::tests::tier_1_anchor_only_diff
    todo_cargo_test todo::tier::tests::tier_1_fanout_uses_sibling_base_when_set
    todo_cargo_test todo::tier::tests::tier_1_fanout_seeds_orphaned_sibling_from_anchor
    todo_cargo_test todo::tier::tests::tier_1_skips_candidates_with_empty_diff
    todo_cargo_test todo::tier::tests::since_override_replaces_anchor_base_for_anchor_only
    todo_cargo_test todo::tier::tests::since_override_errors_when_commit_missing
    todo_cargo_test todo::tier::tests::since_override_errors_when_commit_orphaned
}

#-----------------------------------------------------------------------------
# State database — each function dispatches into the matching cargo
# integration test under `loom-core/tests/state_db.rs`. Sharing the cargo
# binary keeps verify and `cargo test` exercising the same code paths.
#-----------------------------------------------------------------------------
state_db_cargo_test() {
    cargo_run test -p loom-core --test state_db "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_state_db_init — `StateDb::open` creates `specs`, `molecules`,
# `companions`, and `meta` tables and seeds `meta.schema_version`.
#-----------------------------------------------------------------------------
test_state_db_init() {
    state_db_cargo_test state_db_init_creates_tables
}

#-----------------------------------------------------------------------------
# test_state_db_rebuild — `StateDb::rebuild` writes one specs row per
# spec markdown file and one molecules row per active molecule.
#-----------------------------------------------------------------------------
test_state_db_rebuild() {
    state_db_cargo_test state_db_rebuild_populates_specs_and_molecules
}

#-----------------------------------------------------------------------------
# test_state_db_rebuild_companions — `## Companions` parser extracts paths,
# specs without the section contribute zero rows, malformed lines skip.
#-----------------------------------------------------------------------------
test_state_db_rebuild_companions() {
    state_db_cargo_test state_db_rebuild_companions
}

#-----------------------------------------------------------------------------
# test_state_db_rebuild_resets_counters — `iteration_count` returns to 0
# after rebuild, even when previously incremented.
#-----------------------------------------------------------------------------
test_state_db_rebuild_resets_counters() {
    state_db_cargo_test state_db_rebuild_resets_counters
}

#-----------------------------------------------------------------------------
# test_state_current_spec — `set_current_spec` followed by `current_spec`
# round-trips the same `SpecLabel`.
#-----------------------------------------------------------------------------
test_state_current_spec() {
    state_db_cargo_test state_current_spec_round_trips
}

#-----------------------------------------------------------------------------
# test_state_increment_iteration — `increment_iteration` returns the post-
# increment value (1, 2, 3, ...).
#-----------------------------------------------------------------------------
test_state_increment_iteration() {
    state_db_cargo_test state_increment_iteration_returns_updated_count
}

#-----------------------------------------------------------------------------
# test_state_corruption_recovery — opening a non-SQLite blob fails; the
# `recreate()` recovery path replaces the file and rebuild succeeds.
#-----------------------------------------------------------------------------
test_state_corruption_recovery() {
    state_db_cargo_test state_corruption_recovery
}

#-----------------------------------------------------------------------------
# Beads CLI wrapper — each function dispatches into a unit test under
# loom-core/src/bd/client.rs::tests, so verify and `cargo test` exercise the
# same code paths. Tests substitute a `CapturingRunner` to keep the verify
# path independent of a real `bd` binary.
#-----------------------------------------------------------------------------
bd_client_cargo_test() {
    cargo_run test -p loom-core --lib "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_bd_show_parsing — `bd show <id> --json` output deserializes into a
# typed `Bead` value with the expected fields.
#-----------------------------------------------------------------------------
test_bd_show_parsing() {
    bd_client_cargo_test bd::client::tests::show_parses_first_row_into_bead
}

#-----------------------------------------------------------------------------
# test_bd_list_parsing — `bd list --json` output deserializes into a Vec<Bead>
# and the `--status=` / `--label=` filters are forwarded to the CLI argv.
#-----------------------------------------------------------------------------
test_bd_list_parsing() {
    bd_client_cargo_test bd::client::tests::list_parses_array_of_beads
    bd_client_cargo_test bd::client::tests::list_filters_status_and_label
    bd_client_cargo_test bd::client::tests::list_handles_null_response_as_empty_vec
}

#-----------------------------------------------------------------------------
# test_bd_create_returns_id — `bd create --silent` stdout (a single id) is
# parsed back into a `BeadId`; blank stdout maps to `BdError::CreateMissingId`.
#-----------------------------------------------------------------------------
test_bd_create_returns_id() {
    bd_client_cargo_test bd::client::tests::create_returns_id_from_silent_output
    bd_client_cargo_test bd::client::tests::create_errors_on_blank_silent_output
}

#-----------------------------------------------------------------------------
# test_bd_error_handling — non-zero exits map to `BdError::Cli` with the
# argv + stderr captured; malformed JSON maps to `BdError::Decode`; missing
# rows map to `BdError::ShowEmpty`.
#-----------------------------------------------------------------------------
test_bd_error_handling() {
    bd_client_cargo_test bd::client::tests::cli_failure_maps_to_typed_error
    bd_client_cargo_test bd::client::tests::decode_failure_carries_args_context
    bd_client_cargo_test bd::client::tests::show_returns_show_empty_for_zero_rows
}

#-----------------------------------------------------------------------------
# Run-time logging — each function dispatches into a cargo integration test
# under `loom-core/tests/logging.rs`. Sharing the cargo binary keeps verify
# and `cargo test` exercising the same code paths.
#-----------------------------------------------------------------------------
logging_cargo_test() {
    cargo_run test -p loom-core --test logging "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_run_default_output_shape — default render mode prints one header line
# per bead and one short line per tool call (assistant text deltas
# suppressed); the closing line carries tool count + duration.
#-----------------------------------------------------------------------------
test_run_default_output_shape() {
    logging_cargo_test run_default_output_shape
}

#-----------------------------------------------------------------------------
# test_run_verbose_streams_text — verbose mode streams MessageDelta verbatim.
#-----------------------------------------------------------------------------
test_run_verbose_streams_text() {
    logging_cargo_test run_verbose_streams_text
}

#-----------------------------------------------------------------------------
# test_run_writes_per_bead_ndjson_log — every bead spawn writes the full
# AgentEvent stream as NDJSON to
# `<workspace>/.wrapix/loom/logs/<spec>/<bead>-<utc>.ndjson`, regardless of
# terminal verbosity.
#-----------------------------------------------------------------------------
test_run_writes_per_bead_ndjson_log() {
    logging_cargo_test run_writes_per_bead_ndjson_log
}

#-----------------------------------------------------------------------------
# test_run_logs_log_path — opening a sink emits an info-level tracing event
# whose `log_path` field carries the resolved file path.
#-----------------------------------------------------------------------------
test_run_logs_log_path() {
    logging_cargo_test run_logs_log_path
}

#-----------------------------------------------------------------------------
# test_parallel_logs_are_per_bead — running two beads against the same logs
# root writes two distinct files (per-bead, not per-session), and the
# contents never cross-contaminate even when `emit` is interleaved.
#-----------------------------------------------------------------------------
test_parallel_logs_are_per_bead() {
    logging_cargo_test parallel_logs_are_per_bead
}

#-----------------------------------------------------------------------------
# test_log_retention_sweep — `sweep_retention_at` deletes files older than
# `[logs] retention_days` and preserves recent files.
#-----------------------------------------------------------------------------
test_log_retention_sweep() {
    logging_cargo_test log_retention_sweep
}

#-----------------------------------------------------------------------------
# test_log_retention_disabled — `retention_days = 0` disables sweeping.
#-----------------------------------------------------------------------------
test_log_retention_disabled() {
    logging_cargo_test log_retention_disabled
}

#-----------------------------------------------------------------------------
# test_log_retention_failure_tolerance — per-file delete failures (here: a
# read-only directory) do not abort the sweep; survivors and failures are
# both surfaced in the report.
#-----------------------------------------------------------------------------
test_log_retention_failure_tolerance() {
    logging_cargo_test log_retention_failure_tolerance
}

#-----------------------------------------------------------------------------
# Concurrency & locking — each function dispatches into a cargo integration
# test under `loom-core/tests/lock_manager.rs` so verify and `cargo test`
# exercise the same paths. The acceptance behaviour (per-spec serialization,
# 5s timeout, cross-spec independence, read-only commands unblocked,
# init/workspace exclusion, crash recovery) is asserted in those tests.
#-----------------------------------------------------------------------------
lock_cargo_test() {
    cargo_run test -p loom-core --test lock_manager "$1" -- --exact --nocapture --quiet
}

#-----------------------------------------------------------------------------
# test_per_spec_lock_acquired — `LockManager::acquire_spec` creates the per-
# spec lock file and the guard releases on drop.
#-----------------------------------------------------------------------------
test_per_spec_lock_acquired() {
    lock_cargo_test acquire_spec_creates_lock_file
}

#-----------------------------------------------------------------------------
# test_per_spec_lock_serializes — a second mutating command on the same spec
# waits up to 5s and then errors with `another loom command is operating on
# <label>`. The fast contention path (250ms timeout) and the default 5s wait
# are both asserted.
#-----------------------------------------------------------------------------
test_per_spec_lock_serializes() {
    lock_cargo_test second_acquire_times_out_with_spec_busy
    lock_cargo_test times_out_with_default_timeout
}

#-----------------------------------------------------------------------------
# test_cross_spec_no_blocking — locks for distinct spec labels do not block
# each other; both acquire effectively immediately.
#-----------------------------------------------------------------------------
test_cross_spec_no_blocking() {
    lock_cargo_test cross_spec_locks_do_not_block
}

#-----------------------------------------------------------------------------
# test_readonly_commands_unblocked — read-only commands acquire no lock; an
# active spec lock does not block read-only inspection of the workspace.
#-----------------------------------------------------------------------------
test_readonly_commands_unblocked() {
    lock_cargo_test readonly_paths_unaffected_by_spec_lock
}

#-----------------------------------------------------------------------------
# test_init_workspace_lock — `acquire_workspace` errors immediately with
# `WorkspaceBusy` if any per-spec lock is held; succeeds when none are; and
# is exclusive against itself.
#-----------------------------------------------------------------------------
test_init_workspace_lock() {
    lock_cargo_test acquire_workspace_errors_when_spec_lock_held
    lock_cargo_test acquire_workspace_serializes_workspace_holders
}

#-----------------------------------------------------------------------------
# test_crash_releases_lock — a crashed (process-exit) holder leaves no stale
# lock; a fresh invocation acquires immediately. The integration test spawns
# the cargo test binary as a child process, takes the lock, then exits via
# `std::process::exit` so the kernel — not Rust's Drop — releases the flock.
#-----------------------------------------------------------------------------
test_crash_releases_lock() {
    lock_cargo_test crash_releases_spec_lock
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
