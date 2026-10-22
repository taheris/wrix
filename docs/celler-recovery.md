# Recovering the shared host service

If the selected shared Celler/Dolt container is absent or stopped, use the
updated host CLI from any directory:

```bash
wrix service recover
```

Startup prints an absolute recovery command selecting the invoked package
wrapper and recorded runtime. Prefer that command if failed direnv entry leaves
an older Wrix on PATH. No change to the original owner's workspace is required.

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

## Controller policy upgrade

The shared SQL lifecycle uses controller policy epoch 2. An older running
controller can retain cached epoch-1 policy, so ordinary startup refuses to
modify that registry. Stop the old host-controller user service, then run
`wrix service recover` with the updated package. Recovery and updated-controller
startup migrate the existing policies only while holding the original kernel
ownership lock. Existing policies and private issuer keys are retained; do not
remove the ownership file, registry, or keys to bypass this check. An old binary
cannot reopen the upgraded registry.

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
