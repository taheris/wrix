# Services verifiers

## Sandbox-native backend tests

Run `cargo nextest run` in the development shell. Its native-fixture setup
supplies pinned `cellerd`, without requiring a container engine, `unshare`,
mounts, FUSE, TUN or KVM. The development shell also exports `WRIX_TEST_CELLERD`
for direct `cargo test` invocations.

The ordinary `celler_backend` suite now starts real Celler processes against
private temporary databases and archives. It checks registration and repair, RSA
publication credentials and exact-view rejection, retained signing state across
process replacement, and view-scoped rotation. Native Nix dumps create small
archive fixtures without a store connection. Fresh `local?root=...` stores then
import signed archives over real HTTP with signature enforcement enabled;
wrong-key and reachable unsigned controls must fail. No physical-host store,
daemon, configuration or trusted-key list is changed.

Native pull-grant tests use the production RSA issuer and real Celler. They
check exact common/private read sets, one-day expiry, expired/tampered denial,
known-peer narinfo and direct-NAR denial, native worker-upload/management
denial, and fresh Nix imports of publications made after issuance. Readable
native cache metadata contains only the public signing key. The host-controller
issuance API holds its current-policy guard through signing; withdrawal narrows
subsequent credentials without changing old bearers. No credential is
serializable or included in diagnostic types. The Linux worker verifier below
separately exercises production launcher delivery and native multi-view reads.

Pool-issuance cases retain the original checked workspace and serialize current
repository approval with controller registration through a short host issuance
guard. The guest also executes direct-library worker-marker denial in a fresh
chroot containing previously approved host metadata and a forged HOME. These
checks establish neither an authenticated native pool endpoint nor owner,
filesystem export, native roots or installed-store availability.

These are component integration tests, not deployed worker, container mount,
production build sandbox, shared installed-store or performance acceptance. The
native backend VM runs the complete backend suite with ignored cases enabled,
then a real host-policy-withdrawal/fresh-Nix-import suite. It also runs all CLI
service-lifecycle cases and the two simulated containment-lifetime cases that
require host capability. These six host-only cases are ignored in ordinary
Nextest: a worker marker must deny the host API, not be bypassed for tests. The
disposable guest has no worker marker and runs those cases explicitly. This
includes the seven canonical-daemon/native-lock cases: production backend
ownership exclusion, native Rust store access and admitted-hook publication.
Run:

```sh
nix run .#legacyPackages.x86_64-linux.ciApps.test-cache-celler-native-system
```

The Nix-built test executables and scripts are self-contained guest fixtures.
CLI binaries embedded by Cargo remain at immutable output paths; dependency and
incremental build artifacts are not retained. Each isolated-daemon case receives
a private overlaid store, database and real disposable Nix daemon; the
executor's daemon/database are never mounted or modified. Daemon readiness uses
a bounded native `nix store ping`, not a fixed pause. The tests run sequentially
and fail on any unsuccessful case. The isolated-daemon cases deliberately
disable build sandboxing in their disposable stores; they do not accept
production build-user isolation. Platform transport and installed end-to-end
gates remain separate. Heavy service-image packaging inspection is CI-only:

```sh
nix run .#legacyPackages.x86_64-linux.ciApps.test-cache-celler-packaging
```

## Publication-only host configuration

The host bootstrap adds only its retained immutable publication hook. It does
not add shared substituters, signing-key trust, pull tokens,
builder-substitution settings or negative-cache TTL overrides. Existing operator
settings and credential files remain untouched; hooks still require their normal
Nix privilege support. A passive native helper checks effective inherited
settings against retained current/peer key material, native view names, and
known file/loopback endpoint aliases. Unknown metadata fails rather than being
repaired or treated as empty. The helper parses the actual service endpoint
schema; a real CLI conformance test checks the producer/consumer boundary.

The native VM also runs host bootstrap and adversarial signed-artifact tests.
Production Celler serves the signed sandbox positive control. A disposable file
mirror is an adversarial fixture, never a production transport fallback:
deliberately trusting it imports the same malicious bytes, while the generated
production host configuration does not. This proves configuration/signature
exclusion, not correct build execution or a complete first/cached direnv
publication pipeline. The real devshell lifecycle separately inspects effective
Nix configuration inside the guest.

## Verifier boundaries

`specs/services.md` binds Rust-owned contracts to native tests and assembled
transport contracts to `test-ci` apps. Dry-run ENV/MOUNT output and a simulated
Apple inventory are command-planning evidence, not container exposure or guest
SQL evidence.

The real Linux devshell lifecycle fixture starts the packaged Celler owner,
checks that stopping its caller leaves it running, retires and reopens namespace
admission, and requires the same live owner PID and signing-key bytes/inode
throughout. Cache opt-out still starts and stops a separate Dolt-only borrower
without replacing that owner. Readiness observes its hook marker or failed unit
with a bounded cold-image deadline; unit failure is not a successful readiness
signal. This is guest lifecycle evidence, not native worker substitution or
shared installed-store acceptance.

## Waiting-launcher credentials

`test-services-limit-mode-cache-endpoint` now uses native Celler rather than
writing flat cache files. Both real Linux Podman run/spawn paths import signed
host-published fixtures through exact common/private routing. An unrelated
loopback listener remains inaccessible in limit mode; unauthenticated cache
reads fail. The guest cannot create, append or chmod credential files.

The disposable guest advances its own clock to two minutes before the issued
expiry. Each unchanged running worker observes atomic netrc inode/content
replacement through its read-only directory mount and imports a newly published
path after an earlier negative lookup. Native Nix reports negative TTL zero and
the stable netrc path. Completion removes the actual host staging directory;
spawn preserves a NUL-containing stdout payload byte-for-byte, with stderr kept
separate from that protocol stream. The probe exports only public expiry
metadata; the bearer remains in private IPC/netrc and is not written to logs.

The host factory admits an immutable package before service effects, preserves
existing registry policy and registers only absent namespaces. Its per-launch
issuer subsequently calls only authenticated private issuance: refresh does not
start services or register policy. Explicit workspace stop also retires current
read issuance, without terminating the host controller or recalling old grants.
Unit/CLI coverage checks denied refresh, missing package admission, clock
changes, independent sessions, failure/exit behavior and policy retirement.
Test-only management wrappers require a live real controller; they are not
installed systemd/launchd proof. The Linux worker VM uses real packaged
supervision.

This is Linux archive-substitution/renewal evidence, not installed-store reuse,
production build-user isolation, public common population, performance, or
Apple/krun mount acceptance. Those platform gates remain open. The old Darwin
flat-payload probe is not native Celler acceptance.

| Criterion                   | Production seam                                                                      | Existing proof and bounded correction                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| --------------------------- | ------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Shared image installer      | `wrix-service::lifecycle::Runtime::ensure_image` delegates to `wrix-sandbox::image`  | Native image-install tests already cover source dispatch. `service_cache::service_start_uses_shared_image_digest_preflight_without_pruning` now runs the service CLI for both runtimes, on hits and misses, and asserts copy/load dispatch without pruning or retention writes. Live cache/beads apps also install the packaged service image.                                                                                                                                                  |
| Temp cache-only suppression | `Plan::from_parsed`, `start`                                                         | `wrix-service` already tests the suppressed plan. `service_cache::temp_cache_only_start_never_creates_a_service_container` additionally proves the CLI never creates a container, without disabling unrelated runtime status queries.                                                                                                                                                                                                                                                           |
| Cache state layout          | `Paths::for_workspace`, `Plan::ensure_layout`                                        | Native CLI coverage asserts exact current-platform XDG/Library roots, every cache artefact, real Nix key generation, and opt-out without cache state. `test-services-devshell-start-independent` and the retained shell-owned host-Nix tests cover default shell enablement and opt-out wiring. Darwin roots are tested on Darwin, not inferred from Linux simulations.                                                                                                                         |
| Container pull config       | `ServicesState::load_project_cache`, `Plan::env_pairs`                               | Existing `service_lifecycle::cache_guest_endpoint_and_key_reach_both_launch_commands` asserts actual subprocess argv for both launches and nested Loom paths. The Linux `test-services-limit-mode-cache-endpoint` additionally checks effective native Nix settings and signed substitution in both real launch modes.                                                                                                                                                                          |
| Sandbox cache boundary      | Launcher mount construction                                                          | That existing native launch test rejects state/cache/store/daemon/secret mounts. The Linux native worker app proves host-published paths are initially absent locally, rejects cache/daemon exposure, inspects the real read-only credential directory mount and checks staging cleanup.                                                                                                                                                                                                        |
| Cache HTTP endpoint         | Service port leasing, native Celler routes, launcher numeric endpoint resolution     | `wrix-cache::helper_server::static_server_enforces_binary_cache_path_policy` already proves GET/HEAD-only and path policy through a real helper. Native Apple endpoint/address/firewall tests cover guest resolution separately from persisted host publication. The Linux native app proves authenticated Celler access, signed imports, live credential replacement, post-miss publication freshness and unrelated-listener isolation. The legacy Darwin flat-file probe remains unconverted. |
| Dolt platform transport     | Default `DoltTransport`, `ServicesState::load_dolt`, Apple guest endpoint resolution | Native endpoint payload/default-transport/Apple guest-address and SQL authentication tests remain intact. `test-services-dolt-platform-transport` runs the existing beads VM on Linux (real shared Unix socket and guest SQL/sync); on Darwin it runs the live Apple service and authenticates guest SQL through the non-loopback VM address/internal port, also checking host-loopback publication. Host TCP tests are not Darwin guest proof.                                                 |

The seven audited annotations now resolve to native tests or `test-ci` apps;
their obsolete generic services IDs are absent from `.#verify --list`. The cache
and Linux beads apps use the declared direct command-runner fixture, whose
argv/stdio contract is tested by `test_command_runner_package_contract`;
guest-written success markers are required so an agent that exits without
executing the probe cannot pass. Native runtime fixtures have conformance tests.
Container/image commands and test management wrappers are external fixtures; the
owned controller process, private protocol, key generation and Rust
service/launcher logic are real. The legacy lifecycle image-source smoke may
still be run explicitly for its Nix-built source boundary; it is not the
authoritative native image-delegation binding.

Platform/prerequisite skips remain gaps in live execution evidence, not passing
transport results.

## VM execution

The Linux `test-services-devshell-start-independent` and
`test-services-limit-mode-cache-endpoint` CI apps build the fixtures' packaged
NixOS test drivers, then execute their real guest assertions in private working,
scratch and runtime directories. Both `TMPDIR` and `XDG_RUNTIME_DIR` are scoped
to each invocation: the upstream driver otherwise places VM state and VLAN
sockets in shared ambient directories, regardless of its output directory.
Runner preparation does not realize VM test results. The ordinary `systemTests`
derivations remain available for direct Nix builds. Run the heavy service guests
sequentially in memory-constrained agent sandboxes.

Service fixtures select the pinned upstream QEMU module's standard 9p transport
instead of root virtiofsd. This test-only selection needs no host capabilities,
FUSE, TUN, mount operations or KVM. Production container transports and
isolation are unchanged. `services-userspace-vm-smoke` boots a real guest,
verifies both 9p exports, rejects a write to the read-only store export and
exercises the writable shared directory. The generic virtiofsd capability guard
remains tested for executors that actually require it; these 9p drivers do not
declare it.

Guest setup imports exact native registration records for the selected fixture
roots from the executor's private Nix store, then checks the guest's recursive
closure and root hashes. This makes immutable package recipes and already-built
artifacts usable without downloading or building their unrelated build outputs.
It does not mount an executor daemon or database into production containers.

Readiness checks observe units, SQL responses, socket presence and guest-written
markers with bounded deadlines. Caller lifetime uses a blocking FIFO rather than
a sleep. The cache probe uses a smaller package set through the real image and
entrypoint constructors and allows a bounded cold-image TCG execution budget.

Fixtures permit TCG (`requiredFeatures.kvm = false`); missing KVM and container
nesting are not reasons to skip them. Driver preparation, invocation and guest
failures, including exit 77, remain failures. No log text is used to classify a
skip.

A genuine prerequisite gap produces exit 77 and structured skipped metadata.
Loom's declared test-ci capability policy permits that gap for worker-stage
acceptance only. It does not provide live coverage or a push marker; the
integration-branch host-test stage still owns live execution.

`checks.<system>.system-test-prerequisites` exercises the packaged wrapper with
external Nix/driver fixtures and actual kernel preflight.
`verify:prek.ci-batching` also exercises runner preparation, mixed-batch failure
dominance and Loom's worker/host consumption. Fixture passes are conformance
evidence, not passing services VM results.
