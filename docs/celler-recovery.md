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
