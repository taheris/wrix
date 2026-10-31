# Recovering the shared host service

Updating this checkout or reloading direnv does not update the installed host
CLI. First check the selected executable, without changing services:

```bash
printf '%s\n' "$WRIX_BIN"
wrix service capabilities
```

The capability query must print `shared-services-v1`. If it is unsupported,
update the host package through Home Manager, nix-darwin or the user profile. An
explicit or inherited `WRIX_BIN` still takes precedence over PATH; clear a stale
value before re-entering, or explicitly select the updated executable. For
temporary provision from this updated checkout:

```bash
package=$(nix build .#wrix --no-link --print-out-paths)
export WRIX_BIN="$package/bin/wrix"
"$WRIX_BIN" service capabilities
```

The new shell refuses an incompatible legacy CLI before activation. Do not use
an absolute recovery command printed by an older executable: it recreates that
executable's old layout. A retained `loom-…-service` name alone is not evidence
of the old layout; separate Celler and workspace-Dolt processes are.

If the selected shared Celler/Dolt container is absent or stopped, use the
compatible host CLI from any directory:

```bash
"${WRIX_BIN:-wrix}" service recover
```

Startup prints `wrix service recover`. Ensure the compatible host CLI is on PATH
before running it. No change to the original owner's workspace is required.

If an inherited Wrix post-build hook prevents realizing the updated package,
bypass it for that one Nix invocation:

```bash
nix --option post-build-hook '' shell 'github:taheris/wrix#wrix'
```

This bypass does not repair the backend. Recover it explicitly, then re-enter
the project shell to install its current hook configuration.

Recovery is host-user maintenance, independent of repository admission, caller
cache settings and the original owner's checkout. It selects the recorded
runtime and holds host lifecycle exclusion while checking both runtimes,
recreating the selected container using the invoked package's service image,
confirming authenticated readiness, and recording provenance.

`runtime: Running` in status describes the container, not Dolt endpoint health.
If Dolt logs `unix socket set up failed: file already in use` while TCP remains
available, use the updated CLI's `service recover` as well. It briefly stops the
verified shared backend under lifecycle exclusion, retires only a verified dead
Unix socket under the original ownership lock, then resumes it and checks the
selected transport. This interrupts cache and SQL clients during recovery; do
not manually delete socket, database, registration or ownership files. A live or
foreign endpoint is refused, not unlinked.

Recovery preserves the container name, endpoint port, cache data, signing
authority and other persistent service data. An inactive legacy workspace owner
becomes the host-scoped shared backend: recovery does not mount its repository
or open its old workspace database in place. A retained name such as
`loom-…-service` is a legacy runtime identifier, not a dependency on Loom. New
deployments use `wrix-host-service`, independently of the first requesting
repository. Workspace Dolt namespace registration remains the responsibility of
that workspace's ordinary service startup. It joins the existing host backend
rather than creating another container. A stopped shared backend resumes its
existing SQL databases and scoped credentials alongside Celler. No `--host` or
`--no-cache` option is needed or accepted.

Recovery refuses incompatible running owners, competing running backends in
either runtime, uncertain runtime inspection, unverified stopped-container
identity, and missing private signing authority. It does not replace a running
legacy owner. A stopped competing container is not an active owner. Investigate
reported competitors rather than manually removing ownership metadata or
starting another writable backend.

If readiness fails, runtime startup may have succeeded. The existing selection
is retained, while separate host-private candidate metadata records the new
owner's identity for safe retry. Retry recovery after resolving the reported
condition; do not delete the candidate or selection as a workaround. The same
candidate record supports recovery after interrupted initial creation. Recovery
is not a reset, cache prune, signing-key rotation or workspace database
migration.

## Foreign legacy services in other repositories

A running foreign workspace service is not an unavailable shared owner.
Repeating `service recover` cannot resolve it. With the updated host package:

```bash
"${WRIX_BIN:-wrix}" service migrate-legacy
"${WRIX_BIN:-wrix}" service migrate-legacy --prepare
```

Inventory includes both available runtimes, exact workspace/runtime/container,
Podman container identity, retained mounts and possible running consumers. A
selected shared owner and recorded deployment generations are not legacy
competitors. Preparation saves a private, fsynced journal under native
`host-lifecycle/legacy-migration.json`; it does not stop anything. Keep that
journal throughout maintenance and afterward as cutover evidence.

**Obtain maintenance approval before stopping services.** Container inventory
cannot prove host shells, Beads writers, direct SQL sessions or cache clients
idle. Coordinate with the owners of every listed repository and session. Pause
launch automation and old shells; finish or explicitly stop dependent workers.
The command conservatively refuses cutover while any non-service container is
running, including consumers with no workspace label. Mounts help operators
identify their actual checkouts; labels alone are not a dependency census.

After approval and client quiescence, gracefully stop only the inventoried
legacy containers using their reported runtime. Do not stop the existing shared
owner, remove containers/volumes, prune caches, delete `.beads`, unlink native
locks or rotate keys. Retain all mounted storage, each repository's entire
`.beads` tree (including sibling databases), Git administration, native Wrix
state/cache roots, and signing files. Take an independent consistent backup at
this quiescent boundary; archive any container-local writable state as well.
Record pre-cutover Beads counts/history and cache public keys for comparison.

Before cutover, update the installed host package and any explicit `WRIX_BIN`,
PATH, direnv/project pins, post-build hooks and background automation. Clear
shell command caches/reload old shells. An already running old same-user binary
cannot be fenced by a new CLI; recurring running legacy services are detected
and refused, even after an earlier migration was authorized. Diagnostics name
the exact offender and direct the operator back to migration, not recovery.

Once that maintenance and backup work is complete:

```bash
"${WRIX_BIN:-wrix}" service migrate-legacy --quiesced
```

`--quiesced` is the operator's acknowledgment that **all** legacy SQL/cache
clients and old launchers are quiesced; it never stops them on the operator's
behalf. It checks the complete prepared inventory, stopped status, immutable
Podman IDs, mount bindings and canonical checkout paths before authorizing
imports. Missing/recreated/new containers or changed mounts fail closed. Apple
inventory is supported, but automatic cross-repository cutover currently refuses
Apple containers without a supported immutable identity witness; it never treats
their names as sufficient ownership evidence. A legacy selected owner itself
requires separate selected-owner maintenance; this operation handles foreign
services alongside an existing shared owner.

Cutover recovers/checks the selected shared backend, starts each affected
workspace with the invoked host executable, then checks Dolt readiness and cache
status. Existing workspace admission remains required; this is not automatic
admission or reassignment. Existing native import fencing/checkpoint logic
preserves Dolt history, branches and working roots. Sibling databases remain
retained, not silently merged. Legacy cache payloads and signing state stay in
the retained original storage/container; they are not automatically merged into
Celler or rotated. A trust-key conflict is an incomplete migration, not consent
to the destructive rotation described below.

If interrupted or verification fails, resolve the reported condition and repeat
`--quiesced` with the same package/runtime selection. The journal records each
verified repository, but retries recheck every repository and legacy container.
Do not restart legacy services or delete the journal/checkpoints as rollback.
After successful native checks, run `bd status`, inspect representative history
and issues in **every affected checkout**, and compare the recorded counts and
public keys. Verify required cache reads through the intended new client, and
keep the retained legacy cache backup until its consumers and retention needs
are explicitly reconciled. Native readiness is not a claim that all external
clients or all unselected databases were migrated.

## Conflicting workspace cache keys

Host-wide `service recover` does not reconcile workspace trust metadata. If
ordinary workspace startup reports a different key from retained valid namespace
metadata, it deliberately leaves that retained key unchanged. The retained
`cache.secret` file is not authority to replace Celler's current signing state.

The explicit rotation command also works when `celler-publisher.json` was never
installed because startup stopped at that conflict. With the updated host CLI,
run this **from the affected workspace**, only after authorizing loss of that
workspace's existing private cache view:

```bash
cd /path/to/affected/workspace
"${WRIX_BIN:-wrix}" service cache rotate-key
"${WRIX_BIN:-wrix}" service start
```

Rotation invalidates the selected workspace's private view and generates a new
backend signing key. It does not merely adopt the backend key, import a retained
secret key, clear common/peer views, or delete installed store paths. Republish
or warm the private view as needed. Zero pending records or publication roots is
not proof that the backend contains no useful content.

This command requires a selected, identity-verified running backend, existing
host-private issuer authority and admitted workspace scope. If the backend is
absent/stopped, recover it separately first. Missing issuer authority, unsafe
files or bindings to different authority coordinates are refused; do not delete
or edit control metadata to bypass those checks.

If rotation fails after recording its durable intent, backend mutation may have
succeeded. Ordinary startup and automatic publication refuse that pending state.
Resolve the reported filesystem/backend condition and rerun the same command; it
finishes local recording without deleting an already-created replacement view.
Keep the journal and original namespace lock intact. A separate final durability
error can report that the key and authority are already recorded and the journal
removed: do not blindly rotate again in that case. Try ordinary `service start`;
retry rotation only if startup reports a pending rotation. Once rotation
succeeds, a later invocation starts a new rotation and invalidates the view
again.

## Pending deployment updates

A pending update and an explicit shutdown are different admission fences.
`wrix service --host` reports their recorded state; a running backend with
`shutdown: None` and `update: Prepared` is not an explicitly stopped host.
Ordinary startup never clears either fence or upgrades a running deployment.

With current Wrix, an operator can abandon a **provably unstarted** update:

```bash
"${WRIX_BIN:-wrix}" service --host
"${WRIX_BIN:-wrix}" service update --cancel
"${WRIX_BIN:-wrix}" service start
```

Cancellation does not execute the older target package or select the current
package's service image. It retains the exact intent in a private, fsynced
`host-lifecycle/cancelled-update-<attempt>.json` before clearing the active
fence; retry is safe if interrupted after that archival step. All software
roots, native data, signing material, running services and workers stay
retained.

`Prepared` from an older package is **not** proof that no side effects occurred.
For legacy records, including the early `BackendDrain` refusal because a running
backend lacks native handoff support, cancellation additionally checks no native
core generations or installed-store pools, no pending pool maintenance or owner
candidate, a verified still-running original backend different from the target,
and an owned, ready original controller different from the target. Unknown
state, progressed ownership, or later phases refuse cancellation and leave the
fence intact. New update records durably enter `Preparing` before native work;
only the specifically proven legacy refusal can be cancelled from that phase.

Do not have an older `service update` process running concurrently. Current
update and cancellation commands serialize, but an arbitrary older same-user
writer cannot be fenced retroactively. An active current update finishes its
serialized operation before cancellation inspects the resulting state.

If refused, preserve the journals and obtain maintenance appropriate to the
recorded phase; do not delete or hand-edit them. In particular, upgrading a
backend that lacks native handoff still requires separately authorized host-wide
shutdown. Cancellation restores access to the existing selection; it does not
make that upgrade interruption-free. See
[Service Update Command](../specs/cli.md#service-update-command) for
eligibility.

## Controller policy upgrade

The shared SQL lifecycle uses controller policy epoch 2. An older running
controller can retain cached epoch-1 policy, so ordinary startup refuses to
modify that registry. Use the updated host package's explicit maintenance
command:

```bash
"${WRIX_BIN:-wrix}" service repair
```

Repair verifies that native supervision belongs to the recorded controller,
retains the updated package closure, stops only that verified controller,
migrates policy while holding the original kernel ownership lock, updates the
frozen controller package, and confirms authenticated epoch-2 readiness before
recovering the backend. This briefly interrupts controller requests, not native
SQL data service. A foreign unit or an unknown process retaining the ownership
lock is refused, not killed. Ordinary `recover` now checks registry
compatibility **before** creating or starting a backend; it never replaces a
live controller. Existing policies and private issuer keys are retained; do not
remove the ownership file, registry, or keys to bypass this check. An old binary
cannot reopen the upgraded registry.

## Changing the historical container name

This is explicit host-wide maintenance, not a directory-specific setting. First
pause Beads writers and stop the selected backend using its recorded runtime. Do
not remove the container or its volumes. On Podman, for example:

```bash
podman stop loom-398ceb6dac09-service
"${WRIX_BIN:-wrix}" service rename wrix-host-service
"${WRIX_BIN:-wrix}" service repair
```

Rename refuses running owners, name collisions in either runtime, and unresolved
backend recovery candidates. It preserves the runtime, workspace provenance,
image provenance, ports, persistent data and signing authority. A stopped Podman
container is renamed natively, preserving its exact container ID and writable
layer; a missing owner can be retargeted without creating anything. An existing
Apple container is currently refused rather than removed or recreated for a
rename. Backend recovery remains a separate explicit step; use `recover` instead
of `repair` when no controller upgrade is needed.

The transaction records a private journal before touching the runtime. If it is
interrupted, rerun `service rename` with the same target name. Ordinary
lifecycle operations refuse the pending rename rather than accidentally
recreating the old owner. Retry verifies the original native container ID before
committing metadata. Do not delete the rename journal to bypass this check.

Unrelated stopped legacy containers, such as another project's retained service,
are left untouched and no longer block ordinary shared-backend startup. Running
competitors and an inactive selected owner still require explicit maintenance.

## Workspace SQL cutover

On workspace service startup, an offline legacy native database is fenced using
its existing native writer lock, moved into a host-private recovery checkpoint,
and streamed without following symlinks into a new host-private inode graph
before native Dolt backup/restore. This excludes retained legacy directory
handles and stale server auto-connection metadata from privileged native
consumption. This retains commit history, branches, tags, and staged/unstaged
working roots. A running native writer, symlinked source storage, or an
uncertain interrupted restore is refused rather than overwritten. Keep the
checkpoint, journal, and native lock inode intact when investigating a failure.

Workers receive scoped SQL credentials and a read-only socket-directory mount,
not administrator credentials. SQL file access is disabled by immutable startup
configuration. Fixed push/pull goes through the scoped sync broker; its native
file remote is separate from the worker-writable Git snapshot. Host Git
administration stays under host-only control storage, outside both backend and
worker mounts. Worker Git metadata is an independent projection and is never
opened by host Git.

Stopping one workspace retires its native SQL login and sync access without
stopping the shared backend or peer namespaces. An explicit subsequent workspace
start reactivates the retained namespace; client environment queries alone do
not reactivate it.
