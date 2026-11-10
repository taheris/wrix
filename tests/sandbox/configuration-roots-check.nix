{ pkgs }:
pkgs.writeShellApplication {
  name = "check-configuration-roots";
  runtimeInputs = [
    pkgs.coreutils
    pkgs.findutils
    pkgs.jq
  ];
  text = ''
    [[ "$#" -eq 1 ]]
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    export LC_ALL=C
    jq -sc 'if length == 1 and (.[0] | type == "array") then .[0][] else error("configuration cases must be one array") end' "$1" > "$work/cases"
    count=0
    while IFS= read -r record; do
      printf '%s\n' "$record" > "$work/record"
      jq -e 'type == "object" and has("root") and has("expected") and (.expected | type == "array") and all(.expected[]; type == "string")' "$work/record" >/dev/null
      if jq -e '.root == null' "$work/record" >/dev/null; then
        jq -e '.expected == []' "$work/record" >/dev/null
      else
        jq -e '.root | type == "string"' "$work/record" >/dev/null
        root=$(jq -r '.root' "$work/record")
        [[ "$root" == /* ]]
        directory="$root/share/wrix/image-inputs"
        find "$directory" -mindepth 1 -maxdepth 1 -print0 > "$work/entries"
        while IFS= read -r -d "" entry; do
          readlink "$entry"
        done < "$work/entries" > "$work/observed-unsorted"
        sort "$work/observed-unsorted" > "$work/observed"
        jq -r '.expected[]' "$work/record" | sort > "$work/expected"
        cmp "$work/expected" "$work/observed"
      fi
      count=$((count + 1))
    done < "$work/cases"
    printf '%s native configuration-reference cases passed\n' "$count"
  '';
}
