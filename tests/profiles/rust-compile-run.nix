{ pkgs, profile }:

assert builtins.elem profile.toolchain profile.hostPackages;
assert builtins.elem pkgs.stdenv.cc profile.hostPackages;
pkgs.runCommand "verify-rust-profile-compile-run"
  {
    nativeBuildInputs = profile.hostPackages;
  }
  ''
    set -euo pipefail

    export HOME="$TMPDIR/home"
    project="$TMPDIR/project"
    mkdir -p "$HOME" "$project/src"
    cat > "$project/Cargo.toml" <<'TOML'
    [package]
    name = "wrix-profile-rust-probe"
    version = "0.1.0"
    edition = "2024"
    TOML
    cat > "$project/src/main.rs" <<'RUST'
    fn main() {
        assert!(std::panic::catch_unwind(|| panic!("expected probe panic")).is_err());
        println!("wrix-profile-rust-ok");
    }
    RUST

    output="$(cargo run --offline --quiet --manifest-path "$project/Cargo.toml")"
    if [[ "$output" != "wrix-profile-rust-ok" ]]; then
      echo "unexpected Rust profile probe output: $output" >&2
      exit 1
    fi
    mkdir "$out"
  ''
