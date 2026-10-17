#!/usr/bin/env bash
set -euo pipefail

work=$(mktemp -d)
cd "$work"
export HOME="$work/home"
mkdir -p "$HOME" src
printf '[package]\nname="wrix-native-smoke"\nversion="0.0.0"\nedition="2024"\n' > Cargo.toml
printf '#[test]\nfn native_test() { assert_eq!(2 + 2, 4); }\n' > src/lib.rs

[[ "$(id -u)" == 1000 ]]
printf 'runtime uid=%s\n' "$(id -u)"
[[ ! -e /homeless-shelter ]]
[[ "$(< /etc/wrix/private-nix-version)" == 1 ]]
nix path-info --all | sort > initial-paths
export LOOM_NATIVE_PREFLIGHT=rust
python3 <<'PY'
import json
import os
import selectors
import signal
import subprocess
import time

process = subprocess.Popen(
    ["pi", "--mode", "rpc", "--no-session"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, start_new_session=True,
)
try:
    process.stdin.write(b'{"id":"native-readiness","type":"get_state"}\n')
    process.stdin.flush()
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    deadline = time.monotonic() + 380
    pending = b""
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not selector.select(remaining):
            raise TimeoutError("Pi RPC get_state response unavailable")
        data = os.read(process.stdout.fileno(), 65536)
        if not data:
            raise RuntimeError(f"Pi exited before get_state: {process.poll()}")
        pending += data
        lines = pending.split(b"\n")
        pending = lines.pop()
        for line in lines:
            reply = json.loads(line)
            if reply.get("id") == "native-readiness":
                assert reply["type"] == "response", reply
                assert reply["command"] == "get_state", reply
                assert reply["success"] is True, reply
                assert isinstance(reply["data"], dict), reply
                print("Pi RPC get_state: PASS", flush=True)
                break
        else:
            continue
        break
finally:
    os.killpg(process.pid, signal.SIGTERM)
    process.wait(timeout=10)
PY

bash_path=$(readlink -f "$(command -v bash)")
export WRIX_TEST_BASH="$bash_path"
cat > build.nix <<'NIX'
builtins.derivation {
  name = "wrix-native-additive";
  system = builtins.currentSystem;
  builder = builtins.getEnv "WRIX_TEST_BASH";
  args = [ "-c" "printf 'native-additive-ok' > \"$out\"" ];
}
NIX
nix build --offline --impure --file build.nix --no-link --print-out-paths > output-path
output=$(< output-path)
[[ "$(< "$output")" == native-additive-ok ]]
nix-store --check-validity "$output"
nix path-info "$output"

mkdir import-source
printf 'private-store-import\n' > import-source/content
source_store="local?root=$work/import-store"
import_path=$(nix --store "$source_store" store add-path "$work/import-source")
nix-store --store "$source_store" --export "$import_path" > exported-path
if nix-store --check-validity "$import_path"; then
  printf 'import fixture must be absent from the fresh image\n' >&2
  exit 1
fi
nix-store --import < exported-path
nix-store --check-validity "$import_path"
[[ "$(< "$import_path/content")" == private-store-import ]]

# A pinned nixpkgs source is an explicit fixture input, never a host store mount.
if [[ $# -ne 1 ]]; then
  printf 'usage: native-nix-operations.sh /fixture/nixpkgs\n' >&2
  exit 2
fi
export WRIX_TEST_NIXPKGS="$1"
nix develop --impure --expr \
  'let pkgs = import (builtins.getEnv "WRIX_TEST_NIXPKGS") {}; in pkgs.mkShellNoCC {}' \
  --command bash -c 'test -n "$IN_NIX_SHELL"'
nix path-info --all | sort > final-paths
comm -23 initial-paths final-paths > lost-paths
[[ ! -s lost-paths ]]
[[ ! -e /homeless-shelter ]]
printf 'native preflight, Pi RPC, Nix build/import/develop, and state preservation: PASS\n'
