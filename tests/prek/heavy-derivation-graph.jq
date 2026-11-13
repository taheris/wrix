$paths[0] as $nodes |
(if ($nodes | type) == "object" and
    ($nodes | all(.[]; (.references | type) == "array" and all(.references[]; type == "string")))
 then . else error("unsupported store reference metadata") end) |
$derivations[0] as $raw |
(if $raw.version? == 4 then $raw.derivations
 elif $raw.version? != null then error("unsupported derivation metadata version")
 else $raw end) as $drvs |
(if ($drvs | type) == "object" and
    ($drvs | all(.[]; . as $drv | (.outputs | type) == "object" and
      all(.outputs | to_entries[];
        (.value.path | type) == "string" or
        ((.value.method == "flat" or .value.method == "nar") and
         (.value.hash | type) == "string" and ($drv.env[.key] | type) == "string"))))
 then . else error("unsupported derivation output metadata") end) |
($nodes | keys | map({key: (split("/") | last), value: .}) | from_entries) as $byName |
($nodes | keys[] | ["node", .] | @tsv),
($nodes | to_entries[] as $node | $node.value.references[] | ["edge", ., $node.key] | @tsv),
($drvs | to_entries[] as $drv |
 ($byName[$drv.key] // (if $nodes[$drv.key] != null then $drv.key else error("derivation missing from observed closure") end)) as $owner |
 $drv.value.outputs | to_entries[] |
 (.value.path // $drv.value.env[.key]) |
 ($byName[.] // .) |
 select($nodes[.] != null) |
 ["edge", ., $owner] | @tsv)
