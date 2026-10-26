# Loom launch troubleshooting

## Spawn configuration rejected before startup

`invalid SpawnConfig schema` before any tool calls means Wrix rejected launch
input, not that an agent or image failed. Older Loom producers serialize session
fields such as `initial_prompt`, `repin`, and `scratch_dir` at the JSON root.
Wrix accepts those opaque fields only inside `extensions.loom`; workspace, env,
agent arguments, image overrides and mounts remain at the root.

The packaged Loom applies `patches/loom-spawn-extensions.patch` to move session
fields into that namespace; its Direct runner reads the same payload. Keep this
patch until the pinned upstream producer and runner implement that boundary.
`checks.loom-spawn-contract` exercises the patched private-file writer and
runner round-trip against the closed launch-key contract. Rebuild/update the
project's Loom package and re-enter the development shell (or reload direnv) so
the host dispatch uses the corrected producer. Rebuild consumer-supplied Direct
runner images too when applicable. Do not remove Wrix's unknown-field checks or
hand-edit a live private spawn file; those files can contain credentials. Host
Wrix installation advice alone does not repair an incompatible producer.

A subsequent safety-history error is independent: follow the host checks below
without resetting retained evidence.

## Runner input discovery

The packaged Loom backports upstream
[tier-separated batching](https://github.com/taheris/loom/commit/896e6df) and
its [query-count regressions](https://github.com/taheris/loom/commit/db524e2)
through `patches/loom-runner-input-batching.patch`. Discovery queries are shared
per runner group within each tier, without changing System execution semantics
or mixing Check and System declarations. Failed or malformed queries remain
errors, not empty input declarations.

`checks.loom-runner-inputs` runs the patched upstream input-resolution tests.
`nix run .#verify -- cli.browser-gate-scope` checks the consumed CLI against
Wrix's actual verifier inventory, cold browser/image boundaries, and both lanes.
Keep these assertions intact when updating Loom. A full upstream update also
requires an explicit integration of its new checked-definition admission:
input-list JSON and opaque runner commands alone are not executable authority
under that protocol. The bounded backport retains the currently supported
protocol rather than disabling the newer admission check.

## Workspace safety failures

`worker isolation or host safety-history association failed` is a Loom host
safety check, not a Wrix image or service failure. A following filesystem
`No such file or directory` can mean the checkout binding, registered checkout,
registry, or retained history is unavailable.

Run these checks on the **host**, from the original operator checkout, not from
an agent sandbox or a `.loom/beads/` clone:

```sh
pwd
ls -ld .git .loom/integration/.git
ls -l .git/loom-workspace.json .loom/integration/.git/loom-workspace.json
```

The private `loom-workspace.json` files identify a host `registry` directory
outside the repository. Check that directory and its `association.json` and
`history/` are still available to the same host user. A host path such as
`/home/shaun/.local/state/loom-safety/...` is intentionally not available inside
a Wrix worker; do not mount it into the worker to bypass this check.

For a workspace that has **never been associated**, run `loom init` on the host
to establish its integration checkout and safety history. Existing workspaces
created before Loom introduced this association also need that explicit host
initialization. Do not use `--rebuild` merely to address a missing association.

For an **existing association**, restore access to the original registered
checkouts and retained host state. Do not delete bindings, recreate an empty
history directory, copy another checkout's binding, or reset the registry: those
operations cannot recover the original safety evidence. `loom init` deliberately
refuses to replace an existing broken association.

If all those paths exist on the host, report the failing Loom command and which
path is unavailable, along with `loom --version`. Wrix cannot repair or
reconstruct Loom's host-owned evidence from a worker clone.
