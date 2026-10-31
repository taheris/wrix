# Loom source/runtime integration

## Supported boundary

Consumers construct Wrix with the source library:

```nix
wrixLib = import "${inputs.wrix}/lib" {
  inherit pkgs system linuxPkgs;
  inherit (inputs) crane fenix;
  treefmt = imageTreefmtWrapper;
};
```

Use `wrixLib.mkSandbox`, `sandbox.devShell`, `wrixLib.mkProfileImages` and
`wrixLib.serviceImage` for project launch data. Devshells provide the client
selected by that Wrix input alongside generated configuration. Loom dispatch
honors this constructor-owned `WRIX_BIN` after explicit Loom overrides, without
selecting a manifest executable. The host engine and shared controller remain
host-owned; interface failures require explicit maintenance, not a shell-entry
upgrade. See [the consumer workflow](README.md#consumer-workflow). The source
library owns Celler's upstream pin, build recipe and runtime modifications.
Consumers do not intercept its package-manager functions, import patched
derivations, enumerate patches or append individual patches to Crane. There is
no supported consumer replacement of only part of the Celler runtime.

Celler's upstream Nix recipe, Cargo manifests and lockfile are evaluator inputs
from one hash-pinned source tree. Runtime patches and additional Rust modules
are applied during the actual package build, not to an evaluator input. Crane's
dependency-only build intentionally uses its dummy source from the unchanged
manifests/lockfile; it is not the shipped runtime. Changes to those evaluator
inputs require a Wrix packaging update, not a consumer patch-list exception.

This boundary avoids Celler import-from-derivation even when the consumer sets
`allow-import-from-derivation = false`. It does not make unrelated consumer
Fenix channel-manifest fetching IFD-free; Loom's pinned-Fenix adapter is
separate.

Regression coverage:

```bash
nix build .#checks.x86_64-linux.celler-consumer-runtime \
  --option allow-import-from-derivation false
```

The check uses the source library's real Celler package, builds both runtime
patches plus all added modules, and executes the native drain/release and
next-owner/signing-state tests. The ordinary native `celler_backend` suite also
checks key registration and authenticated publication against the packaged
daemon.

## Loom handoff

Apply [loom-celler-source.patch](loom-celler-source.patch) **in Loom**, then pin
Wrix to the published revision containing this fix. The patch removes the entire
obsolete Celler interception, not just its assertion; Wrix now supplies the
complete required runtime. Keep Loom's unrelated pinned-Fenix handling.

```bash
git apply /path/to/wrix/docs/loom-celler-source.patch
nix flake update wrix
nix eval --raw .#devShells.x86_64-linux.default.drvPath \
  --option allow-import-from-derivation false
nix develop --show-trace --command true
```

Evaluation was reproduced with Loom's updated Wrix and nixpkgs pins. The patched
Loom source and fixed Wrix were then evaluated successfully in a disposable
source copy with IFD disabled, preserving the real Loom checkout's local
lockfile changes. Evaluation success is not service activation or admission
success.

## Independent host blockers

`wrix run --dry-run --json` deliberately omits sensitive input values. For an
admission failure, use the updated host executable's
`workspace inspect --checkout PATH` or ordinary `service start`: resolver
process failures now show the selected executable, exit status (or signal
termination), and bounded stderr. Terminal controls are escaped; stdout remains
protocol-only and is not echoed.

During this investigation the host-selected Weld adapter refused Loom with
`workspace is not registered for this host user`. That is independent of Nix
evaluation. Resolve registration through the selected integration's supported
workflow, then explicitly inspect/admit the checkout; do not disable admission
or infer membership from its path.

The running `wrix-faaa94944a8f-service` was independently inventoried as Wrix's
repository-local legacy service, mounting `.beads/dolt`, `.wrix` and its old
binary cache. The separately selected `wrix-host-service` has shared Dolt/Celler
storage. A repository-local legacy name, labels or healthy Beads database do not
make it a verified shared owner. Preserve this refusal. Follow
[foreign legacy maintenance](celler-recovery.md#foreign-legacy-services-in-other-repositories)
with an updated host package, inventory, coordinated downtime approval and
backups. Do not stop peers, delete state, rotate keys, or run cutover merely to
verify evaluation. No services were stopped or persistent payloads removed for
this investigation.

The initially installed executable was
`/nix/store/49nnz6d0pgakgakkbayjz0acc8wx4lg6-wrix/bin/wrix` (wrapping
`/nix/store/hfrhaxqslrnyazkjawsxdpc88kc382v2-wrix-0.1.0/bin/wrix`). It lacked
`workspace diagnose` and `service migrate-legacy`. Updated source-built Wrix was
used separately for diagnostics and passive migration inventory; the host
profile was not replaced. Updating Loom's flake lock cannot update that profile
or upgrade its running services.
