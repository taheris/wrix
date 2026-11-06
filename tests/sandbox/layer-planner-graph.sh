#!/usr/bin/env bash
set -euo pipefail

check_graph() {
  jq -e '
    (.derivations // .) | keys | all(.[];
      test("-(python[0-9.]+-)?(igraph|plotly|narwhals|pyspark|duckdb|pandas|polars|pyarrow)(-[^/]*)?\\.drv$"; "i") | not
    )
  ' "$1" >/dev/null
}

test_layer_planner_graph_audit() {
  local directory name
  directory=$(mktemp -d)
  for name in igraph-1.0 python3.14-plotly-6 python3.14-narwhals-2 pyspark-4 duckdb-1 python3.14-pandas-2 python3.14-polars-1 python3.14-pyarrow-22; do
    printf '{"derivations":{"/nix/store/fixture-%s.drv":{}}}\n' "$name" > "$directory/bad.json"
    if check_graph "$directory/bad.json"; then
      rm -rf "$directory"
      printf 'layer planner graph audit accepted forbidden dependency %s\n' "$name" >&2
      return 1
    fi
  done
  printf '%s\n' '{"derivations":{"/nix/store/fixture-wrix-image-tools-0.1.drv":{}}}' > "$directory/good.json"
  check_graph "$directory/good.json"
  rm -rf "$directory"
}

test_layer_planner_graph() {
  local directory system derivation status
  test_layer_planner_graph_audit
  directory=$(mktemp -d)
  system=$(nix eval --impure --raw --expr builtins.currentSystem)
  derivation=$(nix eval --raw --no-warn-dirty ".#checks.$system.layer-planner-native.drvPath")
  nix derivation show --recursive "$derivation" > "$directory/graph.json"
  if check_graph "$directory/graph.json"; then status=0; else status=$?; fi
  rm -rf "$directory"
  return "$status"
}

case "${1:-}" in
  test_layer_planner_graph_audit) test_layer_planner_graph_audit ;;
  test_layer_planner_graph) test_layer_planner_graph ;;
  *) printf 'usage: %s test_layer_planner_graph[_audit]\n' "$0" >&2; exit 64 ;;
esac
