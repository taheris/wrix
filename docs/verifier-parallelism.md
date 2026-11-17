# Independent verifier jobs

The ordinary dispatcher may run repeated requests for an explicitly
`parallelSafe` owner concurrently. Currently only `cli.shared-verifier-app` is
qualified: its fixture repositories, HOME/state directories and source-cache
snapshots are private. Other owners retain sequential execution.

Each occurrence executes the original dispatcher in a fresh process, including
live admission and the entire owner body. There is no verdict cache or duplicate
suppression. At most four jobs run together. Their stdout, stderr and exact exit
statuses are collected separately and emitted in original argument order,
including interspersed sequential targets. Failure and unverified skip remain
blocking; incomplete batches cannot publish successful results.

The native supervisor owns a process group for each job, checks cancellation and
calling-shell death, and terminates remaining ordinary descendants before
publishing a completed job's status. Its batch deadline is 595 seconds. The
native-suite's separate 300-second acceptance budget is unchanged. Independently
detaching descendants are outside the process-group contract; the qualified
fixture harness separately owns and bounds its isolated subprocesses.

The supervisor observes completion with `waitid(WNOWAIT)`, retaining the group
leader until descendant cleanup finishes, and only then reaps it. This prevents
PID/group-ID reuse between observation and signaling. On Darwin, an EPERM signal
result is accepted only after observing that every member of the exact
still-held group is a zombie; empty, active, mixed or unobservable groups remain
errors. The packaged binary embeds the immutable `ps` executable used for that
observation rather than relying on an ambient runtime tool. Cleanup errors
remain visible and cannot publish a successful job result.

The caller leases the bounded root-manager namespace
`$XDG_CACHE_HOME/wrix/parallel-verifiers` (under `~/.cache` by default). Its
complete manifest contains the dispatcher, selected script and tools. The lease
is inherited by jobs, so killing the supervisor cannot permit a subsequent
replacement to retire roots still held by descendants. Only a current generation
and bounded interrupted replacement are retained, never per-commit roots. Result
files are temporary data, not GC roots.

## SQLite snapshots

Concurrent Nix evaluation can temporarily hold the fetcher-cache database lock.
The native `snapshot-nix-fetcher` command opens only a named
`fetcher-cache-vNUMBER.sqlite` source read-only, waits up to ten seconds for
SQLite lock acquisition, and uses `VACUUM INTO` to produce a consistent
standalone snapshot. It atomically publishes a private temporary file without
overwriting an existing destination. Persistent locks, corrupt inputs and
invalid names remain errors; partial snapshots are not published. WAL snapshots
contain only committed state. No caller service database or admission state is
copied.

Native tests cover held/released locks, persistent contention, WAL isolation,
non-clobbering and corrupt data, concurrent independent exit classes,
cancellation, calling-shell death and descendant cleanup. The owning actual
five-argument dispatcher run returned four independently executed passing shared
owners with an interspersed passing batching owner in the requested order. The
four bodies took 339 seconds concurrently; the complete component run including
tool realization took 497.40 seconds. An earlier 300-second experimental batch
limit interrupted unfinished owners and was correctly rejected. These are
component measurements, not proof of a sub-ten-minute ordinary push.

## Image-producer source isolation

The image and runtime-environment helper packages previously copied unrelated
workspace `lib.rs` and `main.rs` contents. Merely adding a verifier module could
therefore invalidate image producers and their archives. Both helpers now use
the audited standalone tool projection. Package builds/tests retain all selected
tool sources and tests, canonical manifests and lock, but only minimal parsing
entrypoints for unused workspace members. The full-inventory projection remains
the default source API for metadata inspection; full workspace verification
continues to use complete sources.

The source owner compares the actual public helper projections, proves unrelated
target additions preserve the package-scoped source, executes the selected tool
and its integration test, and proves full workspace verification still catches
an added failing unselected test. Actual Darwin and Linux image/environment
source and recipe identities also remained identical after removing an unrelated
verifier binary/test and changing its library implementation.
