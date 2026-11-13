{ pkgs }:
pkgs.runCommand "heavy-derivation-graph-conformance"
  {
    nativeBuildInputs = [
      pkgs.gawk
      pkgs.jq
    ];
  }
  ''
    set -euo pipefail
    printf '%s\n' \
      $'node\t/nix/store/hash-wrix-0.1.0' \
      $'node\t/nix/store/other-output' \
      $'node\t/nix/store/producer.drv' \
      $'node\t/nix/store/left.drv' \
      $'node\t/nix/store/right.drv' \
      $'node\t/nix/store/root.drv' \
      $'node\t/nix/store/clean.drv' \
      $'edge\t/nix/store/hash-wrix-0.1.0\t/nix/store/other-output' \
      $'edge\t/nix/store/other-output\t/nix/store/producer.drv' \
      $'edge\t/nix/store/producer.drv\t/nix/store/left.drv' \
      $'edge\t/nix/store/producer.drv\t/nix/store/right.drv' \
      $'edge\t/nix/store/left.drv\t/nix/store/root.drv' \
      $'edge\t/nix/store/right.drv\t/nix/store/root.drv' \
      $'edge\t/nix/store/root.drv\t/nix/store/producer.drv' \
      $'edge\t/nix/store/clean.drv\t/nix/store/clean.drv' > graph
    awk -v heavy='(^|-)wrix-0[.]1[.]0' -f ${./heavy-derivation-graph.awk} graph | sort > actual
    printf '%s\n' \
      /nix/store/hash-wrix-0.1.0 /nix/store/other-output /nix/store/producer.drv \
      /nix/store/left.drv /nix/store/right.drv /nix/store/root.drv | sort > expected
    cmp actual expected
    printf '%s\n' $'node\t/nix/store/clean.drv' $'edge\t/nix/store/clean.drv\t/nix/store/clean.drv' \
      | awk -v heavy='(^|-)wrix-0[.]1[.]0' -f ${./heavy-derivation-graph.awk} > empty
    test ! -s empty
    printf '%s\n' '{"/nix/store/root.drv":{"references":[]},"/nix/store/output-wrix-0.1.0":{"references":[]}}' > paths.json
    printf '%s\n' '{"/nix/store/root.drv":{"outputs":{"out":{"path":"/nix/store/output-wrix-0.1.0"},"missing":{"path":"/nix/store/not-realized"}}}}' > legacy.json
    printf '%s\n' '{"version":4,"derivations":{"root.drv":{"outputs":{"out":{"path":"output-wrix-0.1.0"},"missing":{"path":"not-realized"}}}}}' > modern.json
    printf '%s\n' '{"version":4,"derivations":{"root.drv":{"env":{"out":"/nix/store/output-wrix-0.1.0","missing":"/nix/store/not-realized"},"outputs":{"out":{"method":"nar","hash":"sha256:fixture"},"missing":{"method":"flat","hash":"sha256:fixture"}}}}}' > fixed.json
    for format in legacy modern fixed; do
      jq -nr --slurpfile paths paths.json --slurpfile derivations "$format.json" -f ${./heavy-derivation-graph.jq} | sort > "$format.tsv"
      awk -v heavy='(^|-)wrix-0[.]1[.]0' -f ${./heavy-derivation-graph.awk} "$format.tsv" | sort > "$format.heavy"
    done
    cmp legacy.tsv modern.tsv
    cmp legacy.heavy modern.heavy
    cmp legacy.tsv fixed.tsv
    cmp legacy.heavy fixed.heavy
    printf '%s\n' /nix/store/output-wrix-0.1.0 /nix/store/root.drv | sort > expected-output
    cmp modern.heavy expected-output
    printf '%s\n' '{"version":999,"derivations":{}}' > invalid.json
    if jq -nr --slurpfile paths paths.json --slurpfile derivations invalid.json -f ${./heavy-derivation-graph.jq} > invalid.tsv; then
      echo 'unknown derivation schemas must not produce an audit pass' >&2
      exit 1
    fi
    touch "$out"
  ''
