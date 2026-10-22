# Recovering stale Celler ownership

If the selected Celler container is absent or stopped, use the updated host CLI
from the original owner's repository and with the recorded runtime:

```bash
cd /home/shaun/src/github.com/taheris/loom
wrix service status
wrix service recover
```

`recover` is explicit maintenance, not an implicit upgrade during `run` or
`start`. It retains host lifecycle exclusion while checking both container
runtimes, recreating the selected container using the invoked host package's
service image, confirming authenticated readiness, and recording provenance. It
preserves the selected runtime, workspace, container name, endpoint port, cache
data, signing authority and other persistent service data. It may restart
cohosted workspace services. Other workspaces cannot take over an unavailable
owner. No `--host` or `--no-cache` option is needed or accepted.

Recovery refuses incompatible running owners, competing running backends in
either runtime, uncertain runtime inspection, unverified stopped-container
workspace identity, and missing private signing authority. A stopped competing
container is not an active owner. Investigate reported competitors rather than
manually removing ownership metadata or starting another writable backend.

If readiness fails, runtime startup may have succeeded, but the ownership record
is retained rather than cleared. Inspect service status/logs and resolve the
reported condition before retrying. This operation is not a reset, cache prune,
or persistent-data migration. Do not delete `celler-owner.json` as a workaround.
