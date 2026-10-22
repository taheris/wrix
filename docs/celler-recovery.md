# Recovering stale Celler ownership

If the selected Celler container is absent or stopped, startup prints an
absolute recovery command selecting the invoked host package wrapper and
recorded runtime. Change to the original owner's workspace named in the
diagnostic, then copy that command. When another checkout encounters the
failure, the diagnostic identifies the owner's workspace hash instead; do not
recover from that other checkout.

Use the printed absolute executable even after failed direnv entry: that failure
can leave an older global `wrix` on PATH, and the new `WRIX_BIN` export may
never reach the calling shell. Do not substitute the raw Rust binary for the
package wrapper: the wrapper retains its service-image and controller bindings.

A stale host that reports `wrix requires --profile-config` on bare `wrix run`
needs updating, not a hand-copied project configuration. Provide the host
package using Home Manager, nix-darwin,
`nix profile add 'github:taheris/wrix#wrix'`, or a temporary
`nix shell 'github:taheris/wrix#wrix'`, then re-enter `sandbox.devShell`. An
explicit consumer `WRIX_BIN` launcher is also exposed as bare `wrix` after
successful shell entry.

If an inherited Wrix post-build hook prevents even realizing the updated
package, bypass it for that one Nix invocation, for example:

```bash
nix --option post-build-hook '' shell 'github:taheris/wrix#wrix'
```

This bypass does not repair the backend. Resolve its reported failure explicitly
and re-enter the project shell to install its current hook configuration; do not
delete cache state or signing keys.

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
