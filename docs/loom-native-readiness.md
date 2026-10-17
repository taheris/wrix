# Loom native Rust readiness investigation

Historical investigation at `ab9baa7a`. The follow-up
[private native Nix policy](private-native-nix.md) addresses these gaps and
records fresh UID-1000 runtime evidence; the blocked outcome below describes the
earlier ancestor-only experiment, not the new construction policy.

## Outcome: blocked on native executor provisioning

Fresh Rust/Pi artifacts from Wrix `ab9baa7a` reproduce the reported exit 78.
Stale artifacts are **not necessary** to explain the failure. Importing Loom's
factory did not make this image ready.

An experimental Wrix image change restored `/nix`, `/nix/var`, and
`/nix/var/nix` to root-owned `0755`. That image passed native preflight and real
Nix/Rust operations as container-root. However, an additional real UID-1000
local-store probe exposed missing writable `gc.lock` and missing secure
build-directory provisioning. **The partial image fix was reverted**, rather
than shipping a root-only success that regresses non-root Nix operations. No
preflight was weakened, live Nix state recursively chmodded, or store/database
deleted.

## Consumed factory and launch path

1. `flake.lock` pins Loom source `e353e043c427d07dd5ed9de1f330bd53109b82d3`.
2. `modules/flake/packages.nix` directly imports that source's `nix/lib.nix` and
   calls `mkProfileManifest` with Wrix's library, Linux image packages, Loom's
   binary, and only the Pi runtime.
3. The factory calls `wrixLib.mkSandbox`. Its `worker-profile.nix` is an
   identity function. Rust's `agentPkg` is a high-priority Pi wrapper that runs
   `scripts/worker-native-preflight.sh` under `LOOM_NATIVE_PREFLIGHT=rust`, then
   execs the actual Pi binary. It does not provision native Nix state.
4. `modules/flake/devshell.nix` exports the manifest. Loom's
   `loom-workflow/src/loop/worker.rs` selects its image/profile config/launcher,
   sets `LOOM_NATIVE_PREFLIGHT=rust`, and passes them through the backend's Wrix
   spawn path. The image entrypoint resolves the checked Pi wrapper on PATH.
5. Loom's own `nix/flake/lib.nix` separately applies
   `nix/patches/wrix-nix-state-ancestors.patch` to its Wrix input. The public
   factory does **not** apply that patch. Wrix's direct import never inherited
   it. The upstream patch also only changes the three ancestor modes.

The pinned native preflight checks all three ancestors' UID/mode, absence of
remote-store overrides/daemon socket and an occupied purity guard, single-user
signed-input policy, private local-store opening, Nix evaluation, and real Cargo
metadata/build/Clippy/Nextest probes. Manifest assertions do not establish these
runtime properties.

## Exact upstream gap and required change

Loom's public factory advertises `native_preflight = "rust"` and installs a
checker, but its native profile transformation provisions nothing. Its private
Wrix ancestor patch is not part of the public factory, and even applying that
patch does not meet the requested non-root contract:

- Baked `gc.lock` is absent. With `/nix/var/nix` correctly root-owned `0755`, a
  non-root writer cannot create it. Existing database/temproots writability does
  not solve this.
- Baked `builds` is absent. Pinned Nix 2.34.9 defaults `build-dir` to the state
  directory's `builds` child. Non-root cannot create that child either.
- Making `builds` world-writable is not an adequate substitute: Nix's local
  store configuration explicitly requires a non-world-writable build directory.

Loom's factory needs an explicit, consumed native-executor provisioning
contract: conforming immutable ancestors; pre-provisioned mutable local-store
coordination files; and a secure effective-executor-owned build directory
selected for the actual launch UID, with safe ancestors. It must preserve the
private store and signed-substitution policy and exercise actual root/non-root
Nix operations. Any necessary image/setup interface should be added to Wrix and
consumed by that factory, rather than a second Wrix-only native provisioning
path or a permissive chmod in the checker. The exact image-vs-launch split needs
upstream agreement; this investigation does not invent one.

## Artifact identities

Both images use launcher
`/nix/store/xlhhvlgndi1qxyfqs17x8ngp07x5078r-wrix/bin/wrix`.

|                   | Fresh original main                                                            | Ancestor-only experiment (reverted)                                            |
| ----------------- | ------------------------------------------------------------------------------ | ------------------------------------------------------------------------------ |
| Manifest          | `/nix/store/fcg5swlcnzlgahn5mgn516zaqr8inc8n-profile-images.json`              | `/nix/store/5sylwkw83p6qmf26hipqbpim90b3gx9c-profile-images.json`              |
| Descriptor        | `/nix/store/28pvwns0ypjniwfqwm8d83dwm47lzhlr-wrix-rust-pi-nix-descriptor.json` | `/nix/store/w5im5lsa57ir3b0xymay5lk79fhzcn2y-wrix-rust-pi-nix-descriptor.json` |
| OCI config digest | `sha256:944f224af12375a7600d7f72f603928bb4d5a2867f0bac838df3e5523322d069`      | `sha256:53bc73806ccad46afb64f9ba3bd41303e761cfc116d8282e3ac4b86cdc37dc0b`      |
| Profile config    | `/nix/store/fr5d5bcl5fb4nrk80456insnbdrbhm9a-wrix-rust-pi-profile-config.json` | `/nix/store/rz3vhicv5ah6xcg6a2vrh3f5xxip331k-wrix-rust-pi-profile-config.json` |

## Commands and results

```sh
nix build .#profile-images --no-link --print-out-paths
python3 tests/sandbox/native-nix-permissions.py <descriptor-oci_layout>
```

The committed metadata regression reads emitted OCI layer tar headers in
application order, checking the three ancestors and retained writable areas. It
fails on the original image's `nix/var/nix` (`0777`) and passes on the
experiment. It is intentionally not installed as a passing flake check while the
production image remains broken. It is not a complete readiness test.

Podman 5.8.9 was provisioned from the pinned nixpkgs. This session is already a
rootless container. Plain nested Podman launch failed mounting `/proc`. Focused
diagnostics used the parent **sandbox's** `/proc` and `/dev` plus its UTS
namespace. These adaptations are not production Wrix launch/isolation proof. No
host Nix store was mounted. The vfs import attempt was abandoned in favor of a
separate overlay store.

```sh
P=/nix/store/4mva69zvgs5qykmqh0vkyf9lzwp16vpw-podman-5.8.9/bin/podman
p() {
  "$P" --storage-driver=overlay \
    --root /workspace/.loom/scratch/wx-gp4r6-podman \
    --runroot /tmp/wrix-podman-overlay "$@"
}
# /tmp/wrix-pull-policy.json contains:
# {"default":[{"type":"insecureAcceptAnything"}]}
p pull --signature-policy /tmp/wrix-pull-policy.json oci:<layout>:latest
p run --rm --cgroups=disabled --network=none --uts=host \
  --mount type=bind,source=/proc,destination=/proc,ro \
  --mount type=bind,source=/dev,destination=/dev \
  --entrypoint /bin/bash -e LOOM_NATIVE_PREFLIGHT=rust -e HOME=/home/wrix \
  <digest> -c 'cd /tmp; pi --help'
```

Original: exit 78, `/nix/var/nix expected 0:755, observed 0:777`. Experiment:
exit 0, actual native build, both Clippy probes, Nextest's native test, then Pi
help. The missing project metadata in `/tmp` was explicitly reported by Loom,
not counted as project verification. The local OCI import policy does not
disable Nix signature checking.

The committed `tests/sandbox/native-nix-operations.sh` adds actual additive Nix
build/output/registration and `nix develop` operations. It also provides a valid
Cargo fixture for native preflight. On a normal host, run this probe in separate
fresh factory images as UID 0 and UID 1000:

```sh
podman run --rm --network=pasta --user=0:0 --workdir=/tmp \
  --mount type=bind,source=<pinned-nixpkgs-source>,destination=/fixture/nixpkgs,ro \
  --mount type=bind,source=<checkout>/tests/sandbox/native-nix-operations.sh,destination=/fixture/probe.sh,ro \
  --entrypoint /bin/bash <image-id> /fixture/probe.sh /fixture/nixpkgs
# Repeat with --user=1000:1000.
```

The experimental image's UID-0 probe passed in a fresh nested diagnostic
container using the proc/dev/UTS options above and
`--network=host -e NIX_CONFIG`. It built a real offline derivation and
substituted signed dependencies from cache.nixos.org for
`mkShellNoCC`/`nix develop`. An earlier network-none offline develop attempt
lacked dependencies and was not counted as a pass.

Nested Podman's single-ID mapping prevents the UID-1000 container leg
(`insufficient UIDs or GIDs`). A full-range `unshare` map also failed. Instead,
the experiment's emitted Nix-state tar members were extracted into a **new,
isolated** `/tmp/wrix-nonroot-fixture` local store (no host database/store
mutation), and host Nix 2.34.9 was executed as real UID 1000:

```sh
setpriv --reuid=1000 --regid=1000 --clear-groups nix \
  --store 'local?root=/tmp/wrix-nonroot-fixture' \
  --extra-experimental-features 'nix-command flakes' \
  --option sandbox false --option build-users-group '' \
  build --impure --no-link --expr 'builtins.derivation {
    name="nonroot-probe"; system=builtins.currentSystem;
    builder="/bin/bash"; args=["-c" "echo ok > $out"];
  }'
```

This fails opening `nix/var/nix/gc.lock` with `Permission denied`. After
creating only that absent fixture lock with mode `0666` (not repairing any live
state), the same command fails creating `nix/var/nix/builds` with
`Permission denied`. This isolated probe disproves sufficiency of the proposed
mode fix; it is not claimed as a full non-root sandbox test.

## Verification and remaining limits

`nix fmt`, `nix build`, `nix flake check`, `cargo build`, workspace/all-target
Clippy with `-D warnings`, and Nextest passed on x86_64-linux. Nextest: 537
passed, 9 skipped. Other Nix systems were omitted. Those repository checks do
not establish readiness; the explicit image regressions expose the gap.

Normal production Wrix/full `loom loop`, a complete UID-1000 sandbox probe, and
Darwin launch remain unavailable here. Current main is **not fixed** by this
investigation; the unsafe partial fix is not retained.
