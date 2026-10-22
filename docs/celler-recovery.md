# Recovering stale Celler ownership

If the selected Celler container is absent or stopped, use the updated host CLI
from any directory:

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
becomes a host-scoped cache backend: recovery does not mount its repository or
restart its Dolt database. A retained name such as `loom-…-service` is a legacy
runtime identifier, not a dependency on Loom. New deployments use
`wrix-host-service`, independently of the first requesting repository. Workspace
Dolt startup remains the responsibility of that workspace's ordinary service
startup. No `--host` or `--no-cache` option is needed or accepted.

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
is not a reset, cache prune, signing-key rotation or database migration.
