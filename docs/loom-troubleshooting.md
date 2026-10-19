# Loom launch troubleshooting

## Spawn configuration rejected before startup

`invalid SpawnConfig schema` before any tool calls means Wrix rejected launch
input, not that an agent or image failed. Older Loom producers serialize session
fields such as `initial_prompt`, `repin`, and `scratch_dir` at the JSON root.
Wrix accepts those opaque fields only inside `extensions.loom`; workspace, env,
agent arguments, image overrides and mounts remain at the root.

The packaged Loom moves session fields into that namespace and its Direct runner
reads the same payload. Rebuild/update the project's Loom package and re-enter
the development shell (or reload direnv) so the host dispatch uses the corrected
producer. Rebuild consumer-supplied Direct runner images too when applicable. Do
not remove Wrix's unknown-field checks or hand-edit a live private spawn file;
those files can contain credentials. Host Wrix installation advice alone does
not repair an incompatible producer.

A subsequent safety-history error is independent: follow the host checks below
without resetting retained evidence.

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
