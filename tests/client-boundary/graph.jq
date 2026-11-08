if .version != 4 then error("expected Nix derivation JSON version 4") else . end
| .derivations as $graph
| ($root | split("/") | last) as $start
| if ($graph | length) > 12000 then error("dependency fixture exceeds bounded graph budget") else . end
| {pending: [$start], seen: {}}
| until(.pending | length == 0;
    .pending[0] as $node
    | .pending = .pending[1:]
    | if .seen[$node] then . else
        .seen[$node] = true
        | .pending += ($graph[$node].inputs.drvs | to_entries | map(select(.value.outputs | length > 0) | .key))
      end)
| .seen | keys
| if any(.[]; test("-(container-[0-9]|swift)"; "i"))
  then error("consumer demands Container/Swift outputs") else . end
