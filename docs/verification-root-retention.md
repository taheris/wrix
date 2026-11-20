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
all managed roots. Paths outside the top level of `/nix/store` and `.drv`
recipes are refused before cache mutation or Nix invocation. A valid recipe is
not an existing output: passing it to `nix-store --realise` starts a build.

Derive the manifest from the same source reference as the actual consumer. For a
Git-flake consumer, use `git+file://` explicitly and compare representative
recipe identities with the ordinary flake CLI.
`builtins.getFlake (toString ./.)` instead captures a whole-directory path
flake, including ignored files; it can retain different recipes and unintended
local data.

Do not automatically retain every transitive build output. Completed outputs
retain their runtime references through Nix. For pending builds, retain the
available prerequisite frontier and source inputs; keep hot dependency caches
explicitly where needed. Retaining completed producers' raw OCI layers as well
as their final archives needlessly multiplies storage. The manifest commands do
not compute this frontier: source identity and the complete manifest remain the
caller's responsibility. Include packages realized dynamically by verifier
bodies, not just their launcher closures. For example,
`test-wrix-cli-in-profile` builds `packages.<system>.image-base` at runtime;
retain that output explicitly even when its CI launcher is already realized.

Replacement registers every requested root with real `nix-store` before
switching `current` and retiring the previous generation. A failed registration
keeps the previous current generation. A subsequent replacement removes
abandoned pending generations under the same lock, bounding repeated failures to
current plus one pending generation. A dry run reports root changes without
creating generations. Unknown files, foreign directories, symlinked generations
and modified root links cause refusal rather than recursive deletion. Each
generation records its requested manifest; indexed root names bind to that
manifest. Nix registration uses batches of 128 paths rather than one subprocess
per root. On the same 4,915-path retained set, initial per-path registration
took 586.99 seconds and batched replacement took 11.65 seconds. These are
cache-management timings, not whole-push or verification timings.

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

## CI batch lifetime

`test-ci` uses the private `$XDG_CACHE_HOME/wrix/ci-runner-state/ci-runners`
namespace (default `$HOME/.cache/wrix/ci-runner-state/ci-runners`). Its
dedicated state parent is created privately without changing the permissions or
ownership of an existing searchable Wrix cache directory. A missing namespace is
initialized atomically; an existing unmarked directory is refused, not adopted.
Each invocation has a separate batch and inherited kernel lease. Only short
filesystem publication and retirement hold the namespace lock; independent
verifier bodies do not share an execution lock.

The native helper replaces its own process with the dispatcher, preserving
stdio, arguments, process groups and real exit/signal behavior. Re-entry checks
the exact open lease descriptor and generated program. The PID label only
prevents recursive wrapping; it never authorizes retirement. Nested invocations
get independent batches while retaining inherited ancestor leases.

An ordinary EXIT closes the dispatcher's descriptor before requesting cleanup.
After forced cancellation or caller death, the next invocation retires only
fully recognized, unleased batches. Live descendants retain their batch even
when their parent is absent. Unknown entries, modified pending roots, foreign
paths and symlinked directories fail closed. Incomplete metadata-only pending
generations can retire without interpreting their truncated manifest; registered
root generations still require exact manifest/link agreement. No recursive
foreign-directory deletion or PID-based stale test is used.

Retirement first publishes a private, bounded, inode-bound record outside the
batch, then atomically detaches the fully validated and exclusively leased batch
with a no-replace rename. The record binds every relative entry, directory/file
identity, regular-file digest and symlink target. It remains until the detached
directory is gone. A killed cleaner can therefore resume after owner, lease or
nested manifest files have already been unlinked, without recreating any of
them. Only the exact recorded remaining subset is eligible: unknown content,
modified bytes/links, replaced directories, escaping paths and unrecorded
detached trees are refused before deletion. Interrupted pre-detach publication
leaves the original batch untouched. Older partially deleted batches without
such a record are not repaired or adopted; they require separate ownership
review.

Before releasing completed recipe links, the dispatcher registers the valid
pending-build frontier using its own already-instantiated recipes. It captures
Nix derivation graph schema 3/4 and output mappings once, without re-evaluating
a different flake identity. Output discovery disables import-from-derivation;
dynamic outputs and unsupported metadata are refused. Each update refreshes
validity by breadth-first layers, stopping at complete existing outputs rather
than rooting every historical transitive build input. Only validated output
paths reach registration; recipes never become `--realise` requests. Duplicate
requests retain their recipe until the final requested execution.

Frontier replacement registers before retirement. If planning/registration
fails, previous roots and recipe links stay in place, remaining requested
verifiers still execute independently, and the batch exits nonzero. Original
receipt order, multiplicity, stdout/stderr and failure/skip handling remain
separate from retention. Capacity thresholds, verifier deadlines, hooks and
approval policy are unchanged; the manager does not run GC.

Legacy temporary roots and older ad-hoc namespaces require separately reviewed
migration. They are not automatically claimed by this tool. The native tests
exercise refusal, failed registration, abandoned generations, concurrent owners,
exit propagation and an actual SIGKILL with a surviving descendant. Real Nix
registration and root visibility are exercised by
`nix run .#verify -- prek.verification-root-retention`. That owner rotates
actual Nix roots, checks failed registration and confirms retirement leaves
store contents intact. It does not collect the caller's store.
