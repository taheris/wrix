# Service-Owned Nix Stores

Persistent installed Nix dependencies and build outputs shared by approved
workers without exposing the physical host store or giving workers store-write
authority.

## Problem Statement

Binary caches supply archives, not persistent installed worker state. Replacing
a worker repeats downloads, unpacking and registration and loses guest-only
builds. A service-owned store reuses installed paths and identical realizations
across concurrent workers and later launches. This is a separate execution and
filesystem capability, not another Celler transport or a remote builder that
copies outputs back into each worker store.

## Architecture

Acceptance: [Pool Acceptance](#pool-acceptance),
[Execution Acceptance](#execution-acceptance) and
[Platform Acceptance](#platform-acceptance).

The existing host-user deployment owns one multi-user Nix installation per
approved canonical repository and guest Linux system. Loom clones use the outer
repository identity. Unrelated repositories never join the same pool. Pool
identity is independent of checkout paths, image versions and service colors.
Separate pools retain separate stores, databases, daemon admission and roots,
while using canonical `/nix/store` paths inside workers and build sandboxes.

Each pool executes in its own service VM, as a backend role of the existing
host-user deployment. The pool-service runtime is independent of the selected
worker runtime: Podman workers remain Podman workers, and krun/Apple workers
retain their separate VM boundaries. Missing service-VM capability is a shared
admission failure, not permission to weaken sandboxing or migrate workers. Dolt,
Celler, proxy and compiler placement is unchanged; their credentials and
host-private authority are not pool-VM bootstrap inputs.

`nixPoolImage` (the `wrix-nix-pool-image` package) is the independent factory
service image, with service-only bootstrap and multi-user Nix configuration
rather than a private worker or trusted-builder template. An image or VM alone
grants neither pool approval nor native client/filesystem admission. Its
bootstrap seeds and pins its immutable runtime before canonical activation;
retained or uncertain ownership requires explicit reconciliation rather than
ordinary-start repair.

`wrix-service` owns pool authorization, the single active backend owner, build
admission, persistent protection and native maintenance. `wrix-sandbox` owns
launch preparation, read-only worker views and completion observations.
`wrix-core` carries parsed pool identities, mode and compatibility contracts;
`wrix-cli` exposes their operations. Nix image producers supply versioned
compatibility metadata and the exact seed closure. No crate or controller gains
a general physical-host Nix executor or arbitrary mount/command interface.

Workers use authenticated, forced-untrusted native Nix clients and read-only
filesystem access to the selected pool. Only the service opens its live
database, locks and build state. Each client and filesystem export is bound to
one launch and pool; a shared listener, root UID or relay is not authorization.
Both planes exclude peer pools and service administration. File transport
preserves existing Podman, krun and Apple worker boundaries; runtime-specific
transports require independent acceptance, not an assumed cross-VM Unix socket
or shared disk.

## Selection and Host Approval

Acceptance: [Selection Acceptance](#selection-acceptance).

The launch modes are `private` and `shared`. Explicit run/spawn choice takes
precedence over repository policy, then the default `private`. Private mode
retains the image-seeded, disposable single-user store and receives no pool
endpoint, filesystem view or prewarm. Mode is independent of Celler enablement
and tool-cache/compiler sharing; neither `--cache` nor `--no-cache` selects it.

`wrix run --nix-store private|shared` follows the existing launcher-option
prefix boundary. Spawn uses optional `SpawnConfig.nix_store.mode`; repository
policy uses optional `[nix_store] mode = "private"|"shared"`. These objects
contain no pool names, paths, endpoints or trust settings. Invalid/unknown owned
fields and values fail before effects, even if a higher-precedence choice would
override them. Omission inherits; choices do not rewrite repository policy.
There is no environment-variable selector or profile-controlled sharing grant.

Shared mode additionally requires host approval for the canonical repository,
compatible versioned image/client/service capabilities and a supported runtime.
Host-created repository identity and guest system identify a pool; neither is an
admission capability. Host approval explicitly binds checked native namespace
members to that repository. A new namespace does not join solely from its Git
common directory or checkout location. Integrated membership uses the original
admitted primary checkout rather than rediscovering scope from path names.
Repository edits, agent arguments and spawn data cannot create host approval or
broaden pool membership. An unapproved, incompatible or unavailable shared
request fails before worker startup with actionable guidance, not a silent
private fallback. Preflight reports the resolved mode, approval and blockers
without allocating state, starting services or issuing credentials.

Public native qualification currently covers `x86_64-linux` Podman and krun.
Apple and other unqualified host architectures remain unavailable before shared
preparation; this does not waive their independent platform acceptance.

Shared mode admits only `network=open`. A shared request under `limit` fails
before preparation; restricted workers receive no Nix build endpoint or export.
Open builds still obey the public-network-only baseline: open is not LAN,
physical-host, VPN or service-management access.

## Store Operations

Acceptance: [Operation Acceptance](#operation-acceptance).

`wrix service nix-store` reports the selected canonical repository's approval,
recorded pools, ownership, protection and compatibility without starting,
registering, creating, repairing or probing a backend. It does not list peer
source paths or credentials. A missing deployment/pool is information, not an
invitation to create one.

`wrix service nix-store enable` records host approval for the selected canonical
repository in an existing deployment; absence directs the operator to ordinary
`wrix service start`. It does not import worker data, start a store or enable
sharing in repository policy. `disable` prevents new shared admissions while
preserving live protection and retained data; it does not promise recall of
previously read bytes or immediate revocation of active mounts. Only
host-private management authority can change approval. These commands have no
caller-chosen pool/path, cross-repository group or `--host` variant.

`wrix service nix-store gc [--wait <duration>]` runs native Nix collection for
the selected repository's existing pools through their already-running owners.
It uses the bounded acquisition, truthful partial results and no-autostart rules
in [Service GC](services.md#gc-availability). Missing/uncertain required owners
are incomplete/nonzero, not empty successes. Collection preserves active and
uncertain-live roots, unrelated pools and service authority; it never deletes
raw store/database files. Protected-only/no-garbage scope can succeed. No
expiry, worker close or ordinary launch automatically removes a pool.

Existing `wrix service gc`, `gc --host`, `gc all` and `gc image` retain their
Dolt/Celler/image scopes; they do not implicitly collect these installed stores.
Celler retention is not Nix GC, and enabling this capability changes no physical
host GC behavior.

## Execution and Confidentiality

Acceptance: [Execution Acceptance](#execution-acceptance) and
[Pool Acceptance](#pool-acceptance).

A pool is one cooperative source/artifact confidentiality and availability
domain. Its admitted clients can inspect stored peer inputs and outputs; native
untrusted Nix maintenance can discard unrooted paths. Root protection supplies
live correctness, not per-worker tenancy, fairness or permanent cache retention.
Disabling future sharing does not erase existing shared data. Dedicated runtime
credentials are never seed inputs or shared pool configuration.

The service is trusted infrastructure under
[Security](security.md#shared-service-trust-boundary). Pool separation limits
ordinary client access, not compromise by cohosted service/compiler code. New
Nix builds themselves use distinct build users and strict build sandboxes
without fallback; the private worker and trusted remote-builder configurations
are not shared-service templates. Builds cannot read service
signing/admin/issuer state, peer workspaces or peer pools. Ordinary and
fixed-output derivations obey the public-network-only baseline, including denied
loopback, private and management destinations. Hash verification after execution
is not egress enforcement. Linux krun pool-service VMs use virtio-net through
packaged passt so guest network controls mediate build egress; TSI is not an
accepted transport for these owners. The runtime binds networking-enabled
upstream libkrun and its packaged passt helper without changing the guest kernel
or worker isolation.

Service-owned finite CPU, memory, PID, concurrent-build and disk-admission
bounds apply before work; worker environment/options cannot enlarge them.
Exhaustion returns a truthful failure without killing peer work or bypassing
protection to free space. No per-worker hostile-tenant guarantee is implied.
Cancellation or a lost client is not proof that native build work has ended.

Daemon trust is checked end to end, including root and relayed clients. Repair,
signature bypass, trusted-user options and unsigned input-addressed replacement
remain denied. A flag named forced-untrusted is not sufficient evidence. Seed
imports use host-managed startup authority, never worker administrative trust.
Pool builds do not gain host substitution authority, host signing keys or Celler
publication rights. Initial pool substitution uses independently trusted
upstreams, not a union of repository-private Celler views or service-held cache
signing trust.

## Bootstrap, Protection and Lifecycle

Acceptance: [Image Acceptance](#image-acceptance),
[Protection Acceptance](#protection-acceptance) and
[Lifecycle Acceptance](#lifecycle-acceptance).

Every image retains the private-Nix constructor contract. Shared readiness is an
additional versioned image/client/service capability, not a raw profile override
or reinterpretation of `privateNix`. Trusted startup seeds and pins the exact
required image closure before a worker store mount hides baked binaries. This
includes entrypoint, interpreters, agent, profile and configuration references.
Private provisioning, reserved paths/environment and numeric ownership guards
remain intact. Service bootstrap software does not depend on a hidden or empty
pool store. Seeding never replaces a live database with image registration or
recursively repairs ownership. Old live image closures survive new-image
launches.

Normal `nix build`, `nix develop`, `nix run`, out-links and profiles work
without root-equivalent client trust or per-worker NAR reimport. Service-side
protection covers image paths, evaluation/build inputs and live resulting
shells/programs. Protection is acquired before native temporary protection can
be released; periodic after-the-fact rooting is insufficient. Client paths
cannot select arbitrary service/host root destinations. Pin records survive
daemon/controller restart; protection is released only after authoritative
worker exit and native work completion. Launcher loss, disconnect, elapsed
leases and quiet sockets are not exit evidence. Unknown readers retain roots and
block unsafe maintenance.

One writable owner and export lifetime are tracked per persistent pool across
service generations. Registering another repository does not replace serving
infrastructure or require broader worker mounts. Startup resumes compatible
recorded owners; it cannot upgrade a running owner. Explicit host update
accounts for every provisioned pool in addition to the four core service roles.
The initial contract rejects owner/export replacement while any worker, build or
uncertain reader still uses it, before switching that pool or discarding pins.
After confirmed drain, ownership release precedes candidate acquisition and
native readiness. No concurrent Apple disk attachment, forced unmount, replay,
implicit database migration or claimed transparent reconnect is allowed. Failed
handoff remains nonzero with truthful retained ownership/protection. Workspace
stop preserves peers; explicit host shutdown preserves pool data and roots for
safe reconciliation before readmission.

## Success Criteria

### Selection Acceptance

- Run/spawn resolve the independent strict mode and reject unapproved,
  incompatible, unsupported and restricted shared requests before effects.
  [test](../crates/wrix-sandbox/tests/shared_nix.rs::invocation_overrides_store_policy_but_never_skips_strict_policy_parsing)
  [test](../crates/wrix-sandbox/tests/shared_nix.rs::shared_store_under_limit_is_blocked_even_with_capability_metadata)
  [test](../crates/wrix-sandbox/src/command/launch/nix_store/mod.rs::tests::qualification_is_bound_to_actual_linux_architecture_and_runtime)
  [system](test-ci:test-shared-nix-public-podman)
  [system](test-ci:test-shared-nix-public-krun)
- Preflight reports mode, approval and required preparation without mutation.
  [test](../crates/wrix-sandbox/tests/shared_nix.rs::store_preflight_reports_mode_without_preparing_or_starting_shared_work)
  [test](../crates/wrix-cli/tests/workspace_admission.rs::nix_store::approved_shared_preflight_is_passive_but_execution_still_requires_runtime_acceptance)

### Pool Acceptance

- Explicit host approval binds checked namespaces to stable repository
  identities without implicitly enrolling linked checkouts, changing peer
  decisions or provisioning a pool.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::nix_store::linked_checkouts_share_one_repository_only_after_explicit_membership_and_disable_together)
- Canonical clones share only their approved repository/system pool; ordinary
  peer-pool reads and host-management requests fail with known-live controls.
  [system](test-ci:test-shared-nix-public-podman)
  [system](test-ci:test-shared-nix-public-krun)

### Operation Acceptance

- Actual CLI dispatch implements passive information, host-only enable/disable
  and scoped bounded native GC without broadening existing GC commands.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::nix_store::installed_store_information_is_passive_and_enable_never_creates_a_deployment)
  [test](../crates/wrix-cli/tests/workspace_admission.rs::nix_store::store_collection_rejects_invalid_scope_and_wait_arguments_before_host_effects)
  [test](../crates/wrix-cli/tests/workspace_admission.rs::nix_store::unavailable_store_collection_never_provisions_or_reports_empty_success)
  [system](test-ci:test-shared-nix-public-podman)
  [system](test-ci:test-shared-nix-public-krun)

### Execution Acceptance

- Root and relayed clients remain untrusted with repair, signature and option
  negative controls against the actual admitted endpoint.
  [system](test-ci:test-shared-nix-podman)
  [system](test-ci:test-shared-nix-krun)
- Actual strict build-user sandboxes enforce filesystem, ordinary/fixed-output
  egress and service-owned resource bounds without production fallback.
  [system](test-ci:test-shared-nix-podman)
  [system](test-ci:test-shared-nix-krun)

### Image Acceptance

- Factory image and launch metadata agree on independent shared capability and
  exact seed closure while preserving private-Nix provisioning and layering.
  [check](test-ci:test-shared-nix-image-contract)
- The dedicated pool-service image seeds and pins its runtime before canonical
  activation, preserves those paths through native GC, enforces strict
  multi-user builds despite inherited policy overrides, and refuses retained
  ownership without changing the native database.
  [system](test-ci:test-shared-nix-pool-bootstrap)

### Protection Acceptance

- Production root handoff and native GC preserve live inputs, out-links,
  profiles and shells across client loss and daemon/controller restart.
  [system](test-ci:test-shared-nix-public-podman)
  [system](test-ci:test-shared-nix-public-krun)
  [system](test-ci:test-shared-nix-krun)

### Lifecycle Acceptance

- Production ownership/update transitions reject live or uncertain replacement
  and retain data and pins across safe drain, failed candidate and restart.
  [test](../crates/wrix-service/src/nix_store/owner/backend/test.rs::update_requires_recorded_release_before_any_candidate_effect)
  [test](../crates/wrix-service/src/nix_store/owner/backend/test.rs::released_metadata_without_native_checkpoint_cannot_acquire_a_candidate)
  [test](../crates/wrix-service/src/nix_store/lifecycle/test.rs::fresh_maintenance_cannot_capture_an_uncertain_stop_as_release)
  [system](test-ci:test-shared-nix-krun)

### Platform Acceptance

- Two fresh concurrent Podman workers and a replacement reuse installed paths,
  realize an identical derivation once and use real Nix/Rust workflows while
  denying mutation and preserving live GC protection.
  [system](test-ci:test-shared-nix-podman)
- Independent krun workers satisfy the same reuse, workflow, isolation and
  lifecycle controls across their actual VM boundary.
  [system](test-ci:test-shared-nix-krun)
- Independent Apple workers satisfy the same controls with authenticated,
  server-enforced read-only export, dynamic visibility and measured I/O.
  [system?](test-ci:test-shared-nix-apple)

## Requirements

### Non-Functional

1. Warm installed dependencies incur no repeated upstream NAR fetch/import in
   fresh workers; identical concurrent derivations build once per pool. This is
   not zero filesystem traffic or perpetual retention after unrooted GC.
2. Platform acceptance measures upstream NAR requests/bytes, imports, build
   invocations, filesystem I/O and elapsed time with fixed images/lockfiles and
   paths added after worker startup, including prior negative lookups. It runs
   real develop/build/run/profiles, selected checks, Cargo build, Clippy and
   Nextest; cache-hit claims alone are insufficient.
3. Each runtime's absent capability, skip or timeout is missing evidence, never
   another runtime's pass. Partial platform delivery does not enable unsupported
   transports. These requirements bind to
   [Platform Acceptance](#platform-acceptance).

## Out of Scope

- Physical-host store/daemon mounts, runtime-control sockets or host key trust.
- Writable worker stores/databases shared between independent owners.
- Cross-repository pools, strict intra-pool secrecy or hostile-tenant isolation.
- Shared mode for restricted networking; no automatic private coercion.
- Worker runtime migration, live-mutating OverlayFS lower layers or shared live
  ext4 block attachment across VMs.
- Privileged-service shortcuts, a custom Nix fork or protocol allowlist proxy
  without a separately authorized design.
- Worker Celler publication, host artifact promotion or automatic personal-host
  cache seeding.
