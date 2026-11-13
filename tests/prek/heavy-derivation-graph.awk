BEGIN { FS = "\t" }
$1 == "node" {
    name = $2
    sub(/^.*\//, "", name)
    if (name ~ heavy && !marked[$2]) {
        marked[$2] = 1
        queue[++last] = $2
    }
}
$1 == "edge" { dependent[$2, ++count[$2]] = $3 }
END {
    for (first = 1; first <= last; first++) {
        node = queue[first]
        for (position = 1; position <= count[node]; position++) {
            parent = dependent[node, position]
            if (!marked[parent]) {
                marked[parent] = 1
                queue[++last] = parent
            }
        }
    }
    for (node in marked) print node
}
