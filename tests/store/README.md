# Pool-service bootstrap and transport

## Host storage preparation

Host preparation uses checked controller registration and repository approval to
reserve a private per-pool slot. Its durable reservation and native lock do not
claim VM readiness. Disk allocation happens after the short approval guard is
released, reserves eight GiB through native preallocation with one GiB of free
headroom, and writes the versioned public bootstrap input only after sync.
Caller loss, partial allocation and retained or substituted paths require
explicit reconciliation; ordinary preparation never adopts or erases them. The
resource-intensive Rust allocation test is executed explicitly:

```bash
cargo test -p wrix-service native_allocation_reserves_full_capacity_and_retains_single_owner_metadata -- --ignored
```

The live krun verifier rejects a sparse backing file before initialization, then
boots with host-preallocated storage. It checks that formatting, native builds,
GC and drained shutdown preserve the backing inode and full allocated capacity.
Formatting disables discard and reestablishes allocation after metadata zeroing;
filesystem logical size alone is not disk admission. These are preparation and
guest-boundary checks, not production worker delivery or restart acceptance.

## Recorded host VM ownership and native maintenance

```bash
nix run .#legacyPackages.x86_64-linux.ciApps.test-shared-nix-host-owner-linux
```

The checked controller API provisions a dedicated rootless krun VM from the
package-selected pool image. Host tools are independently package-bound and
GC-rooted. Rootless UID-mapping helpers and KVM are existing platform
prerequisites; no worker runtime change, privileged container or host policy
repair is performed. Only the selected pool's data leaf enters that VM;
management private keys, software roots and ownership records remain outside.
The manager records creation intent before runtime creation, native identity
before start, and serving state only after pinned-host, forced-command SSH
confirms the native protocol. It checks image configuration identity (not the
OCI manifest digest), guest architecture, labels, exact runtime and mounts.
Caller/controller object loss neither stops the VM nor permits a new owner.
Ordinary reuse authenticates the recorded owner, not the newly invoked image.
Unknown creation or ownership transitions remain fenced rather than repaired.

The host API's drained stop requires native acknowledgement or uncertain
transport followed by actual runtime exit zero, the synchronized guest release
checkpoint and runtime removal before recording release. Resume retains the
recorded image/runtime, verifies old-runtime absence and preserves the pinned
server key. Unexpected exits do not authorize resume. Scoped CLI collection uses
only recorded running owners through the same authenticated channel, retains
host exclusion through native completion, retries busy acquisition within its
per-pool budget and never realizes images or starts a VM. Native GC execution
has no acquisition-derived running deadline. Information reports the recorded
owner phase, immutable image configuration digest and retained
execution-protection count without probing a runtime or claiming observed
readiness or compatibility. The protection inventory checks the durable
execution index against every retained client record: interrupted registration,
attachment and retirement remain counted; released history is not live
protection. Missing indexed records, foreign pools and substituted metadata fail
rather than reporting zero readers. A recorded zero is not native work-drain or
maintenance authority.

The local-API Rust live test provisions real storage and a VM, drops the
controller, collects through a separate actual CLI even after approval is
disabled, reuses the same native owner, confirms unavailable collection after
stop, then resumes recorded software with a new runtime identity and drains it
again.

The pool-side explicit update coordinator additionally records the exact target
pool image and package-bound runtime in the durable maintenance receipt. Repair
cannot reinterpret an interrupted update or silently resume old software. The
complete owner census and target-runtime preflight precede any stop; candidate
acquisition starts only after clean native release. Candidate roots use a new
private leaf without replacing the old software roots. Provisioned stopped pools
are included in update activation, while controller repair preserves their
stopped state.

Guest upgrade uses a separate version-four bootstrap input bound to the previous
sealed software and unchanged administrator key. It consumes the clean release,
requires the same Nix version before database access, imports and pins the new
startup closure incrementally, preserves existing pins, and reseals the new
bootstrap before daemon readiness. It does not format retained storage, replace
the database, repair ownership or permit ordinary resume to change software.
Interrupted upgrade retains its consumed checkpoint and host admission fence.
These are pool coordinator and guest implementation boundaries, not a claim that
the whole-deployment update/shutdown CLI or Apple transport is implemented. The
Rust live maintenance exercise now includes update/retry and old-root retention;
that extended live exercise has not been run in this environment.

A second Rust live test starts the actual packaged host-controller process and
uses its authenticated private protocol. A one-use request is published only
under the original checked scope and approval; its private frozen observation
includes the original primary, resolver selection/membership, Git
administration, namespace and host generation. Re-attestation compares all
retained observations; a namespace/hash or fresh path discovery alone cannot
authorize provisioning. Consumption rechecks native approval and scope in a
short host writer window. No host paths, pool selection, program or forwarding
inputs cross the protocol. The client probes this separate preparation interface
before publishing intent. Requests are not worker credentials, exports or
lifetime evidence.

The controller bounds outstanding jobs and runs scope re-attestation and native
preparation outside its bounded request loop. Concurrent preparation for one
pool serializes without blocking other pools or private status requests. A real
host writer lock holds consumption while the requester queues and exits; status
remains pending and no pool is allocated. After release, two independently
checked requests prepare/authenticate the same owner. The ordinary async client
API is also exercised. Stopping the actual controller process preserves that
native owner for authenticated later maintenance. Finished-job eviction or
controller restart can make a status unknown; neither releases native ownership
nor proves that a worker or build exited. Interrupted native phases remain
fenced.

Execution preparation adds a separate private interface. Its one-use intent
contains an original native execution reservation, not a caller-selected pool,
public key, export path or runtime ID. Missing, foreign, cancelled and already
handed-off reservations cannot issue clients. A durable per-pool index precedes
client creation; losing an indexed directory is uncertainty, not an empty pool
or permission to recreate a grant. Client state, keys and view leaves remain
outside guest data and store contents. Each key authenticates only native
forced-untrusted Nix and the jailed read-only SFTP export.

The controller owns a read-only, nodev/nosuid FUSE projection with unique native
mount identity. Exact mount source/type/readonly flags are checked, including on
reuse. Native paths survive the private wire protocol as bytes. The package
supplies SSHFS; an existing, owned setuid OS FUSE helper is a checked platform
prerequisite. No permissions are repaired or privileges installed. Retirement
uses only that fixed helper and selected leaf, never force/lazy unmount.

Bounded retirement observers retain protection until host-native terminal
evidence appears. Explicit revocation of a still-reserved ticket atomically
prevents the native launch handoff; it cannot cancel launching or bound work.
Public execution reconciliation remains conservative, and foreground completion
is not terminal evidence. Worker retirement still needs the existing exact-ID
inactive/removal protocol. The observer gracefully unmounts its own projection,
then requires authenticated native work drain and release before dropping the
host index. Busy/unknown readers and interrupted creation/unmount/release remain
protected. Startup reattaches observers to recorded clients without starting,
resuming or adopting any VM, grant or projection. Native GC and shutdown reject
unreleased or inconsistent host client ownership in addition to guest
protection.

The live Rust test exercises private-protocol execution grants, real
forced-untrusted Nix realization, read-only visibility, automatic cancelled-peer
projection retirement without disrupting a live peer, controller-process loss
with a surviving projection, same-owner reuse and proof-gated drain/revocation.
No worker runtime is substituted. These initial checks remain host/client
components, distinct from the actual Podman worker components below and from
production run/spawn acceptance.

Worker preparation adds a separately probed private interface and freezes the
selected immutable image configuration digest and shared-Nix metadata alongside
the original reserved ticket. Actual installed image architecture, Linux OS,
managed/profile/private/shared labels and the exact immutable closure metadata
hash are checked before provisioning. Seeding completes and is authenticated
again before any worker-purpose key or projection is issued. A requested image
is descriptive input, never a caller-selected endpoint, mount or program.

Authenticated image inspection distinguishes missing registration from a
complete exact durable pin set. Interrupted records, altered/extra/missing pins,
substituted state and unexpected native validity refuse; they are never adopted
or repaired. Only genuinely missing native paths are exported and imported.
Already installed paths receive the new image's exact durable pins without any
archive staging or import, while an already complete image returns before host
root/archive creation. A bounded native importer disk-buffers individual NARs;
upstream legacy import buffers whole NARs in memory. The fixed trusted startup
role uses native trusted-import semantics, never a worker signature exception.

The real packaged-controller test seeds an actual factory worker-image closure
before returning its client/view, reuses it for another checked request, pins a
distinct actual image with the same closure without any transfer/import, and
preserves both complete registrations through native GC and sealed VM resume.
These remain component checks: no normal launched-worker or runtime/platform
acceptance is inferred.

### Podman worker components

The separately probed Podman preparation interface freezes the selected native
filesystem plane in the original checked intent. Fixed package tools create,
inspect and gracefully unmount its FUSE source inside Podman's own rootless
mount namespace. The source must not propagate into the host namespace. This
avoids leaving an untracked source copy in Podman's persistent namespace after
host-side unmount. Retiring a legacy host-only view also checks for a native
reference to that exact export/device in Podman's namespace and gracefully
unmounts only that verified full-filesystem reference. A durably retiring source
can continue after confirmed unmount; unexpected identities or subdirectory
references remain fenced. Native mount ID/device/source and
readonly/nodev/nosuid flags are checked in the owning namespace; unknown
namespaces or identities retain protection rather than being adopted. Host-only
component views retain SSHFS symlink containment. Worker sources preserve
canonical Unix links, interpreted inside the worker filesystem, and use
immutable installed-file caching while negative lookups remain uncached.

Each worker receives only five fixed readonly configuration/key files, its
readonly store bind and an exact role-port firewall exception through the
existing Podman host-loopback mapping. Trusted image startup establishes a
bounded worker-local Unix socket, forwarding raw native daemon streams through
pinned SSH with that execution's forced-untrusted key. No host daemon/socket,
management key or general forwarding destination is supplied. Unix native-store
semantics permit local execution of installed programs; an ssh-ng store alone
cannot implement native `nix run` or `nix develop`. Startup authenticates native
`trusted=false` before any workspace/agent code. Reserved mount destinations and
the client bootstrap cannot be shadowed by profile or caller mounts.

The live test now creates two independent actual Podman workers and a
replacement through the SDK containment path. Before starting either object it
checks the immutable native image digest and readonly runtime mounts. They run
actual build/develop/run/profile operations, see paths added after an explicit
negative lookup and reuse the same installed result. Mutation, repair, unsigned
input-addressed import and administration are rejected through the actual worker
endpoint; a known-readable physical-host file remains inaccessible even through
a pool-output symlink. Killing the replacement's foreground launcher leaves
native containment and GC protection live. Exact inactive inspection, removal
reply and independent confirmed absence precede terminal proof, namespace-local
graceful unmount and authenticated native work drain. Closing records remain
protected while peer native work/readers prevent global drain. The
private-controller path also proves checked Podman preparation and automatic
retirement of a never-started reservation without a host-namespace mount.

These are real worker/SDK components, not normal CLI run/spawn, full
Rust-workflow/performance acceptance, service-unit restart, krun-worker or Apple
acceptance. Elapsed worker milestones are recorded; complete upstream/import,
build-invocation and filesystem-I/O measurements remain required. Native command
helpers use one async thread and bound native GC marker threads rather than
exhausting the fixed control PID budget with per-connection runtimes.

Failed tests retain their private fixture rather than deleting possibly live
backing data. Production run/spawn factory integration, full host-wide
update/recovery, independent runtime/platform and complete lifetime/performance
acceptance remain outstanding; shared launches stay disabled.

## Krun worker preparation

The private controller has a distinct krun preparation probe and an immutable
`krun` worker purpose. It freezes the original checked scope, execution ticket
and image binding before seeding or client issuance. Older controllers fail that
probe before an intent is published. A Podman grant cannot stand in for a krun
grant or vice versa. Host-factory admission checks the package's controller,
pool image and native tools bindings plus existing projection prerequisites
before credential issuance or execution-journal creation. Krun admission also
checks its fixed runtime and existing KVM access. Passive previews do not
perform these native checks or start supervision.

The krun source uses the same server-enforced read-only SFTP export and private
Podman mount namespace, but the worker is still launched through the existing
separate-VM krun entrypoint and identity mapping. Preparation checks the fixed
runtime executable and existing KVM access; it adds no device, capability,
privileged mode or worker-runtime migration. The executable comes from the
recorded owner's protected package, not an ambient `krun` alias. The launcher
rechecks that selection with the source and rejects changed, missing or
duplicate runtime arguments before handing off the native reservation. Agent
arguments remain after the immutable image boundary. Native containment
retirement uses the recorded object and same independently checked
namespace-local source.

This is implementation wiring, not a cross-VM transport or platform pass. Public
shared admission remains disabled, including krun's unsupported-runtime blocker.
Actual separate-VM path visibility, networking, lifetime, workflow and
performance acceptance remain required; no unavailable transport silently falls
back to ordinary Podman or a private store. Native tests cover frozen intents,
failure cancellation and exact runtime command binding without launching a VM.

## Explicit same-image guest resume

Bootstrap version 3 adds a separate `resume` storage input. Normal allocation
and preallocation still refuse retained installations. Resume requires the
sealed original bootstrap closure and admin public key, the same Nix version,
private canonical metadata, a clean release checkpoint, no lease records and no
interrupted resume marker. It consumes release evidence before mounting the
retained filesystem, verifies bootstrap pins, and neither formats, reseeds,
rotates keys nor repairs ownership or export configuration. Failed/interrupted
initialization cannot replay the old checkpoint. Only another drained shutdown
publishes a new release checkpoint.

The live krun verifier removes the old runtime after native exit zero and clean
release, exercises missing-checkpoint and changed-software refusals, then starts
the same image against the retained backing file. Native registration metadata,
installed derivations/results and the pinned SSH host key survive; released
client grants do not return. A newly admitted client uses the read-only export
and existing realization before collection and a second drained shutdown.

This is a guest restart primitive, not host reconciliation authority. The host
must independently prove runtime/storage release before attachment; metadata or
a quiet socket alone never permits concurrent VM access. Production supervision,
worker delivery, update transitions and platform acceptance remain separate.

## Host package selection

```bash
nix run .#legacyPackages.x86_64-linux.ciApps.test-shared-nix-host-binding
```

The host package and its controller carry an explicit dedicated pool-image
binding, separate from the core service image and project launch configuration.
The actual packaged CLI reports this target passively through
`service nix-store`; it does not realize the target, create control state or
provision a pool. Missing bindings remain unavailable, and raw pool-image
environment aliases are rejected rather than becoming alternative software
selectors. The packaging verifier runs that binary from two different project
directories/configurations and verifies the same target and absent host-state
effects.

## Apple image identity prerequisite

Apple's native image descriptor ID is not the OCI configuration digest declared
by the package. For its current inspection format, runtime image lookup reads a
private, versioned configuration receipt bound to the exact native descriptor
and guest architecture; absent evidence is not a digest match. Installation
obtains that evidence by exporting the selected Linux variant, reading its OCI
configuration digest and rechecking the native identity before atomic receipt
publication. Warm lookup only reads the receipt: it neither exports the image
nor creates cache paths. Foreign, substituted or malformed receipts fail closed.

The Rust image tests cover descriptor identity parsing, receipt binding and
permissions, symlink refusal and passive missing evidence. Native Apple export
and installed-image acceptance have not been run here. This prerequisite does
not implement Apple pool ownership, authenticated read-only projection, worker
handoff or whole-deployment lifecycle dispatch.

## Guest bootstrap

Run the dedicated image inside a disposable Linux VM:

```bash
nix run .#legacyPackages.x86_64-linux.ciApps.test-shared-nix-pool-bootstrap
```

The verifier consumes the factory image's filesystem and environment. It pivots
away from the test guest's init root and unmounts that root before bootstrap;
chroot alone is unsuitable because it prevents the user namespaces required by
strict Nix sandboxing.

It checks argument, UID, mode, alias and retained-state refusals; inherited Nix
policy isolation; an untrusted native peer's rejected privileged overrides; a
strict unprivileged build with a readable-outside/hidden-inside filesystem
control; native GC preservation; and retained-owner refusal without database
changes. The native cgroup policy limits CPU, memory and PIDs, while one build
user bounds concurrent native builds. A preallocated 8 GiB ext4 backing contains
store data, logs and build scratch space. The verifier exercises disk exhaustion
and a cgroup OOM, then confirms the native daemon and existing output survived.

Build-network policy attaches cgroup BPF hooks before native admission. Its UID
selection uses the guest kernel's build identity, including inside Nix user and
network namespaces. Live controls cover a reachable public-address fixed-output
build and denied IPv4/IPv6 loopback in fixed-output and ordinary builds. The
service pins public DNS resolvers before work admission instead of inheriting a
runtime's private resolver. A conformance-tested DNS fixture resolves owned
public and loopback controls; the actual build can reach the public answer but
cannot connect to the private answer. The live krun verifier also resolves a
real public upstream from a fixed-output build.

The image also runs a root-owned lease controller and public-key-only SSH on
port 2222. Startup requires a private `bootstrap.json` in the service data root,
containing version `1` and the host administrator's canonical Ed25519 public
key. Administration is a fixed JSON operation, not a shell. Registered launch
keys reach only forced-untrusted native Nix sessions or read-only SFTP inside a
filesystem jail. SSH forwarding, PTYs, user configuration and passwords are
disabled. Native GC exclusion covers outstanding leases; release first revokes
new admission and refuses completion while native clients/builds remain.
Inspection, collection and shutdown return busy immediately when native state is
held by other work; they do not queue behind a seed import and consume an
unbounded maintenance-acquisition budget. Acquired collection still has no GC
execution deadline.

The controller duplicates the supervisor's native GC-lock open description,
rather than reopening its pathname. A live fault control stops the supervisor,
kills and observes the controller's death, and confirms independent kernel GC
exclusion remains before allowing fail-stop to proceed. This is not the full
host/worker lifetime-handoff acceptance gate.

Live transport checks cover pinned host authentication, worker-key rejection by
the administrator account, privileged repair and unsigned input-addressed import
denials, fixed-command enforcement, read-only export, a readable outside-jail
control, live-lease GC refusal, release and native collection. Trusted startup
can import an exact staged image archive and pin its declared paths; this is not
a worker signature exception. Management and transport processes share the
aggregate resource budget and have a bounded control subgroup. The VM has 4 GiB
guest RAM and two guest vCPUs, with an independently finite 6 GiB host-container
budget for hypervisor and backing-file I/O cache. Its runtime profile is
recorded and preserved on resume; older recorded limits are not silently
enlarged. The native aggregate limit is 3 GiB/256 PIDs, with work limited to 1.5
GiB/144 PIDs, trusted seeding to 768 MiB/16 PIDs and control to 384 MiB/96 PIDs.
Seeding OOM is group-local, and seed activity participates in quiescence and
clean-release checks.

## Automated native interruption acceptance

On a clean, published Linux checkout with the existing rootless Podman, KVM and
FUSE prerequisites, run one command; no milestone watching or manual signal is
needed:

```bash
nix run .#legacyPackages.x86_64-linux.ciApps.test-shared-nix-interruption-linux
```

The app runs a focused packaged-controller test: one pool, one worker-image
installation and two real SDK workers. It does not first run the broad
requester-loss, maintenance, seed/resume or factory-lifetime scenarios; the
normal host-owner app still runs those unchanged. Authenticated setup phases
(controller, image, each worker and worker evaluations) appear in progress
output. Image import and native boot still take real time. It waits at most ten
minutes for an atomic, invocation-nonce and fixture-inode-bound checkpoint after
both real workers finish their initial Nix evaluation. Before signalling its
owned supervisor with SIGTERM, it independently requires the pool and both
workers to be running and their SSHFS exports to exist. Progress is printed at
least every fifteen seconds during readiness, teardown and namespace inspection.
The checkpoint itself expires after two minutes if the harness disappears.

A read-only witness joins and retains the **original** fixture Podman namespace
before interruption. After bounded supervisor cleanup (three minutes), private
session helpers are terminated through checked pidfds while the session leader
remains unreaped, preventing PID/session reuse from targeting unrelated work.
The witness inventories containers, images and mounts again in the same retained
namespace, even after Podman retires its pause process. A replacement empty
namespace or a teardown receipt alone cannot prove absence. Host mount absence
is checked separately. Cleanup failures, early exits, readiness timeouts,
hanging helpers and namespace changes fail the harness; they are not
interruption passes. No force/lazy unmount or broad host-container deletion is
performed.

Before signalling helpers, and again on their ten-second graceful-stop timeout,
the harness saves PID/parent/group/session, executable, command line, signal
masks, thread states and kernel wait channel. Proc directory descriptors pin
observations against PID reuse; unavailable fields retain explicit errors.
Failure cleanup keeps the original namespace witness alive and attempts the
final namespace and host inventory even after a helper or supervisor failure.
Partial inspection replies are retained too. Resource absence after fallback
cleanup does not turn a failed graceful stop into a pass.

The app always retains its fixture and private registry. Full native output,
namespace snapshots, packaged and checkout Git revisions, exact test command,
observed native exit (expected 143), harness exit and result are saved under the
printed `.loom/scratch/linux-acceptance/interruption-*/` directory. Optional
`--output /absolute/new-directory` selects a new evidence directory; timeout
options are intended for diagnosis, not relaxed acceptance. Native prerequisite
failure or a dirty/unbound app build cannot pass. Portable stub contracts and
Rust checkpoint tests exercise the mechanism but are not native acceptance.

## Native test cleanup and retained fixtures

The host-owner tests register their disposable roots before native allocation. A
parent guard tears down native resources on success and assertion unwind; the
packaged runner also has an invocation-scoped registry and exit/signal traps
around bounded test subprocesses. Cleanup stops containers in that fixture's
isolated Podman storage, gracefully unmounts its exact Wrix SSHFS exports in the
host and Podman namespaces, and removes stopped containers and fixture images.
Before releasing the fixture's Podman namespace, it makes remaining disposable
storage owned and writable by the host caller from inside that user namespace;
rootless image layers may otherwise contain subordinate-UID or read-only files.
Unexpected nested storage mounts prevent this step. No host privilege is added.

A private, root-inode-bound receipt records completed resource teardown before
any directory deletion. File cleanup preserves that receipt, the marker and
runtime directory until last, so a partial deletion can be retried without
starting Podman against missing runtime state. The native assertion subprocess
has a private process group: its guard terminates owned helpers before fixture
teardown, not just the assertion process. External VM teardown remains scoped to
the fixture's isolated Podman store; it does not force-unmount exports.

Successful runs remove fixture files. Failed or interrupted runs still stop VMs
and unmount exports, but retain the fixture and invocation registry for
diagnosis. The helper attempts bounded container-state and last-200-line log
captures under private `native-cleanup-diagnostics/` before removing containers;
unavailable captures are reported and do not prevent resource teardown. Retained
backing disks still consume host space. Cleanup failure also fails the run and
retains its paths for retry. It never changes production uncertainty guards or
touches the user's normal host controller. An uncatchable kill or host crash can
still require recovery.

After stopping native test runs, remove legacy retained test resources from the
host, as the same user who ran the tests:

```bash
python3 tests/store/cleanup-native-fixtures.py --discover --remove-files
```

Discovery is restricted to owned legacy `/tmp/.tmp*` fixtures identified by Wrix
SSHFS mounts, pool-container monitor arguments, or retained native-pool control
state. Explicit roots require a native-test marker or the checked legacy layout.
A private invocation registry can also identify a fixture whose marker was
removed by older partial cleanup. Symlink escapes and roots writable by other
users are refused. No force/lazy unmount is used. Without `--remove-files`,
teardown retains diagnostic files; it does not reclaim the fixture's backing
disks. A refusal reports the retained path and error instead of treating a
missing cleanup capability as success.

Portable conformance and parent-guard regressions run in the ordinary checks.
Successful mocked runtime teardown is not native Podman/FUSE acceptance; run the
host-owner verifier on a suitable Linux host for that boundary.

## Linux service-VM export

```bash
nix run .#legacyPackages.x86_64-linux.ciApps.test-shared-nix-pool-export-linux
```

This separate live verifier requires rootless Podman, KVM, `/dev/fuse` and an
existing working `fusermount3`. It installs the public OCI descriptor under a
fixture-only reference and boots krun without extra OCI capabilities, privilege
or security-option overrides. Only the fixture's private data directory is bound
into the VM; the physical host store and daemon are not mounted. It uses
loopback-only runtime port publication, pinned SSH host authentication and a
read-only Host FUSE view. A prior negative lookup becomes readable after native
realization without remount or per-client import. Mutation fails, collection is
blocked by the live lease, and release remains busy until the filesystem session
closes. An actual untrusted native GC request is observed waiting on the native
GC lock, then cancelled; live data survives and subsequent native drain must
confirm that no orphaned collection/client work remains. An authenticated
shutdown is refused while a lease remains. After drain, it closes admission,
targets the observed daemon through a pidfd, and waits for native work's kernel
cgroup to become unpopulated. The supervisor stops control processes,
synchronizes store state and records release. The verifier requires observed VM
exit zero plus that checkpoint, not a `stopping` reply or a lost SSH connection.
Ordinary startup still refuses retained state; this adds no implicit
reconciliation or restart permission. The separate controller-loss fixture must
not produce the successful-release checkpoint.

The jail binds its VM's existing null device; VirtioFS cannot create character
devices in the persistent data directory.

This checks the real service-VM/export transport, not a production worker launch
or Podman/krun worker acceptance. It neither supplies Mac evidence nor enables
shared mode.

This is a guest-service component verifier, not installed-store reuse, host
launch authorization, host-owner disk admission/supervision, lifetime handoff,
run/spawn delivery, or Podman/krun/Apple platform acceptance. Those contracts
have separate gates in `specs/store.md`. Shared launches remain disabled.
