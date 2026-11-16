# Verification cache retention

Agent-created Nix roots are caches, not test evidence. An indirect root remains
live until its filesystem link disappears; normal Nix GC cannot infer that a new
commit supersedes an old image matrix. Appending a new rooted directory on every
retry retains every historical closure. A process killed with SIGKILL also
cannot run an EXIT trap.

`wrix-verification-roots` manages an explicitly initialized private directory.
It does not scan, adopt or delete existing root namespaces. In particular, it
never touches profiles, service roots, Loom-owned roots or the Nix store itself.
The command is part of `wrix-verify-tools`, not the public host-service CLI.

```sh
# Run once, under a caller-owned parent directory.
wrix-verification-roots init /absolute/private/cache

# A manifest contains one existing top-level /nix/store path per line.
wrix-verification-roots plan /absolute/private/cache current-outputs.txt
wrix-verification-roots replace /absolute/private/cache current-outputs.txt

# Keep the same cache leased throughout verification/publication.
wrix-verification-roots run /absolute/private/cache current-outputs.txt -- git push origin main
```

The manifest is an explicit complete replacement set, not an append operation.
Use outputs and prerequisites of the selected current build, not an ever-growing
history or all outputs in the store. Include any intentionally retained rollback
or qualification outputs explicitly. An empty manifest intentionally releases
all managed roots. Paths outside the top level of `/nix/store` are refused.

Replacement registers every requested root with real `nix-store` before
switching `current` and retiring the previous generation. A failed registration
keeps the previous current generation. A subsequent replacement removes
abandoned pending generations under the same lock, bounding repeated failures to
current plus one pending generation. A dry run reports root changes without
creating generations. Unknown files, foreign directories, symlinked generations
and modified root links cause refusal rather than recursive deletion.

The lock is kernel-owned and inherited by `run`'s actual command and
descendants. A killed parent does not authorize retirement while a descendant
still holds the lease. Concurrent management refuses immediately. Managed
commands that close inherited descriptors or independently detach work are
outside this lease contract: do not use this wrapper to protect such background
work. This is cooperative same-user cache management, not a boundary against a
malicious user rewriting the cache directory or lock. Keep the parent private.

Logs, manifests and acceptance receipts belong outside this cache. The tool
emits exact retired link/target pairs; capture those separately if an audit is
needed. It does not invoke GC, mint verdicts or approvals, change hooks, or
equate cache availability with test acceptance. GC remains an explicit operator
action after reviewing the retained set. Summing closure sizes double-counts
shared paths; use a union/deduplicated store audit and filesystem free-space
measurements for capacity decisions.

Legacy temporary roots and older ad-hoc namespaces require separately reviewed
migration. They are not automatically claimed by this tool. The native tests
exercise refusal, failed registration, abandoned generations, concurrent owners,
exit propagation and an actual SIGKILL with a surviving descendant. Real Nix
registration and root visibility are exercised by
`nix run .#verify -- prek.verification-root-retention`. That owner rotates
actual Nix roots, checks failed registration and confirms retirement leaves
store contents intact. It does not collect the caller's store.
