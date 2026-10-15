# Host Services

One service container per host user, hosting logically isolated workspace Dolt
and project Nix caches, a shared compiler daemon and per-launch egress policies.

## Problem Statement

Wrix workspaces need long-lived local services that are shared by host commands
and sandboxed containers without exposing the host filesystem, host
`/nix/store`, or host Nix daemon to agents. The service lifecycle hosts Dolt,
Celler-backed workspace cache views, the enforced egress proxy and the shared
sccache compiler daemon. Nix-cache transport/signing/publication/retention
belong to `cache.md`; tool-cache modes and compiler selection belong to
`profiles.md`.

## Architecture

`cli.md` owns root command grammar and dispatch. This spec owns one shared
service-container identity per host user and the operations behind
`wrix service ...`; it is not a machine-wide multi-user service. Physical
container ownership is independent of repository paths. Workspace namespaces
remain keyed by canonical repository identity, with Loom bead/integration clones
using the outer repository identity and non-repository paths using their
canonical path. Namespace ports, state and locks remain distinct. Repository
cache trust remains private except for the common dependency view explicitly
authorized by [Cache Visibility](cache.md#cache-visibility).

Acceptance: [Host Service Acceptance](#host-service-acceptance) and
[Compiler Service Acceptance](#compiler-service-acceptance).

The service container owns four service families:

- **Dolt for beads** — the Dolt SQL server for the Beads-owned workspace
  database. `beads.md` owns issue tracking and database storage; this spec owns
  only the shared container lifecycle and endpoint publication.
- **Project Nix cache** — Celler runs inside this container, with scoped
  read-only client views and host-authorized publishing. `cache.md` owns cache
  contents, transport, signing, publication and retention; this spec owns
  placement, namespace isolation and endpoints.

- **Egress proxy** — shared HTTP/CONNECT infrastructure with isolated per-launch
  grants. `proxy.md` owns request admission, authentication and network safety;
  this spec owns its placement and service lifecycle.
- **Shared compiler** — one actual sccache daemon for eligible shared Rust
  launches under [Shared Compiler Service](#shared-compiler-service), not a
  generic HTTP blob-cache backend or full worker-runtime migration.

### Shared Compiler Service

Acceptance: [Compiler Service Acceptance](#compiler-service-acceptance).

The actual shared sccache daemon is cohosted with Dolt, Celler and the proxy,
with service-owned persistent compiler-cache data. It does not use personal-host
sccache data, another HTTP cache backend, or an additional compiler
container/VM.
[Profiles Shared Compiler](profiles.md#shared-rust-compiler-service) owns
shared/private selection and the restricted-Rust admission rule. One native LRU
cache defaults to `SCCACHE_CACHE_SIZE=50G` for the host-user daemon. Its
capacity is service-owned, not a per-workspace allocation or reconfiguration
from worker/compiler environment, repo preference or spawn input. Local private
and personal-host cache settings remain independent; no new public budget setter
or aggregate tool-cache quota is introduced.

Admitted requests need the selected guest-platform compiler/toolchain, correct
source/working-directory/environment view and writable originating outputs.
Simultaneous workspaces and distinct clones must not collide in a common
`/workspace` or output mapping. This is not a common Cargo target directory.
Input/output registration must preserve the narrow host mount boundary: no
physical-host store/daemon, runtime socket, blanket home/worktree parent or
host-private issuer/publishing/management state. Selected compiler views do not
authorize arbitrary host paths or personal-cache imports.

Shared-compiler compatibility is part of required-interface admission, without
allowing one workspace to replace a running shared container. Private/restricted
launches do not register compiler inputs or receive its execution endpoint.
Read-only information/preflight does not start the daemon or allocate its data.
Workspace retirement removes only its compiler access/work; it does not stop the
shared daemon, clear pooled data or interrupt other workspaces' requests.
Explicit host shutdown affects all shared compiler availability like the other
service families.

Configure the managed daemon for caller-independent service lifetime rather than
native idle exit tearing down the shared infrastructure. A compiler job's normal
nonzero result is not a daemon failure and must not stop Dolt/Celler/proxy.
Unexpected active-daemon exit follows the shared fail-fast contract.
[Security](security.md#shared-service-trust-boundary) owns the explicitly
accepted compiler-code/service-state compromise boundary; no separate compiler
isolation or worker-equivalent per-job resource accounting is promised.

### State layout

Acceptance: [Host Service Acceptance](#host-service-acceptance).

Host-private management, publishing and credential-issuer state is separate from
service-visible workspace data and Celler's delegated cache-signing state.
Workspace-private state roots remain platform-native:

| Platform | Durable state root                                                        |
| -------- | ------------------------------------------------------------------------- |
| Linux    | `${XDG_STATE_HOME:-$HOME/.local/state}/wrix/workspaces/<workspace-hash>/` |
| Darwin   | `$HOME/Library/Application Support/wrix/workspaces/<workspace-hash>/`     |

`<state-root>/services.json` records namespace endpoint metadata, port leases
and its metadata schema version. `cache.md` owns host-private namespace
publication/pending/status state, public-key metadata and the separately managed
Celler signing/archive data.

Service-visible workspace data roots are:

| Platform | Workspace service-data root                                                            |
| -------- | -------------------------------------------------------------------------------------- |
| Linux    | `${XDG_STATE_HOME:-$HOME/.local/state}/wrix/service-data/workspaces/<workspace-hash>/` |
| Darwin   | `$HOME/Library/Application Support/wrix/service-data/workspaces/<workspace-hash>/`     |

`beads.md` owns the live database and canonical sync-worktree children beneath
this root. The shared container mounts managed service data and Celler's
persistent metadata/archive data plus admitted compiler input/output views, not
arbitrary repository trees or the host home. Celler's managed mounts permit its
own metadata/signing/archive writes. New workspaces register without replacing
the container or changing its mount set. Tenant-facing APIs expose only
authorized workspace data and endpoints; a shared parent mount is not permission
for clients to traverse other tenants.

Celler receives cache-signing authority in this shared boundary under
[Cache Authority](cache.md#cache-authority). Authoritative publish manifests,
host publishing/management credentials and credential-issuer private authority
remain outside service-visible mounts. Direct sandbox mounts and ordinary
endpoint/credential delivery expose neither Celler signing state nor those
host-private roots, other workspace filesystem roots, the host `/nix/store` or
the host Nix daemon socket. Authorized common-cache reads are API access, not
mounts of peer workspace data. These delivery/API guarantees do not isolate
service-visible state from delegated compiler code under
[Security](security.md#shared-service-trust-boundary). A sandbox may receive its
own narrow Beads sync-worktree mapping under the contract in `beads.md`, not the
all-workspaces service-data root. Workspace-local `.wrix` pointers are
non-authoritative and contain no management credentials.

### Lifecycle

Acceptance: [Host Service Acceptance](#host-service-acceptance).

`mkDevShell` ensures the shared service and registers the selected namespace
when its project cache is enabled or Beads is configured. Nix-cache opt-out does
not disable Beads, proxy or shared compiler use. Both sandbox launch modes
ensure required namespace services; shared Rust also requires its compiler
service even when the Nix cache/Beads are off. `limit` requires the shared
proxy, with Rust using private tool caches rather than shared compiler
delegation. Registration and shared-container creation are concurrency-safe and
idempotent. A malformed registration cannot overwrite another workspace's state
or policy.

Service lifetime is caller-independent: shell/editor exit does not kill the
shared container. Ordinary `wrix service stop` retires only the selected
workspace's services and proxy grants, leaving other workspaces and shared
common dependency view available. `wrix service stop --host` is the explicit
host-user-wide shutdown and revokes all service availability and proxy grants.
Bare `wrix service` reports the selected workspace; `wrix service --host`
reports shared-container state without requiring a repository. Both information
defaults are read-only even when infrastructure is absent/stopped: no namespace
registration, managed-state creation, image installation, service startup/repair
or grant issuance. Logs and endpoint queries remain workspace-scoped. Host
operations select the current user's shared infrastructure, not another machine
or elevated authority. The container remains available until explicit host
shutdown; no idle timer is introduced.

Cache-only registration remains suppressed for temporary scratch workspaces.
Loom clones share their outer repository namespace without creating separate
containers. A requested restricted launch may still use shared proxy
infrastructure in a scratch workspace. Proxy startup/registration failure
prevents restricted agent launch; later proxy failure never enables direct
egress.

### Compatibility and Replacement

Acceptance: [Compatibility Acceptance](#compatibility-acceptance).

Ordinary workspace startup (`mkDevShell`, `run`, `spawn` and
`wrix service start`) reuses a compatible running host service. Compatibility
must be positively established for the required service interfaces and policy
features; unknown or unverifiable compatibility is not permission to replace it.

An incompatible running service causes nonzero failure with explicit host-wide
remediation. Startup does not remove/recreate it, install a replacement image,
alter managed database/cache data, rewrite endpoints or revoke peer proxy
grants. A workspace stop does not authorize host replacement. The explicit
replacement flow is `wrix service stop --host`, then `wrix service start`; the
shutdown is deliberately disruptive to all namespaces, but retains managed
workspace data and valid Celler signing state plus host-private
publishing/management authority. Old traffic grants do not survive replacement.

Creating a missing or fully stopped service is normal startup, not replacement
of a running service. Read-only launch preflight reports a required incompatible
service as a blocker without repair; a stopped service remains a preparation
need. Compatibility is rechecked by actual startup, not authorized by a prior
preview.

### Service image source

Acceptance: [Host Service Acceptance](#host-service-acceptance).

The shared host-user service image is a wrix-managed Nix-built image.
`image-builder.md` owns its source metadata and ownership labels, while
`sandbox.md` owns installation and explicit image collection through shared
runtime-image mechanics. Service startup still installs/reuses required images
but does not initiate broad pruning or impose an MRU-eight eviction threshold.
Temporary import-reference cleanup remains installer housekeeping. This spec
owns only the service lifecycle's use of the shared installer; image commands
belong to [CLI Image Commands](cli.md#image-commands).

### Service operations

Acceptance: [Host Service Acceptance](#host-service-acceptance).

`cli.md` owns public command placement, root help behavior and the packaged
binary surface. This spec owns workspace information/start/stop/GC, explicit
host information/stop/GC, logs/endpoints, bare Dolt information and Dolt
endpoint/readiness operations (`socket`, `port`, `host`, `wait`,
`sandbox-endpoint`). [CLI Dolt Information](cli.md#dolt-information) owns the
information default replacing status and the print-only attach helper.
[CLI Service Scope](cli.md#service-scope) owns information defaults and the
`--host` selector for dual-scope operations, including data GC. The duplicated
host namespace and retired status/cleanup spellings are not aliases. Ordinary
workspace start remains the ensure path, including after explicit host shutdown;
the scope flag adds no host-start/logs/endpoints workflow or management
authority.

`cache.md` owns `config`, `status`, `publish`, `warm`, `rotate-key` and
[Retention Control](cache.md#retention-control) under `wrix service cache ...`.
Retention information/set/reset default to the canonical workspace's private
policy; `--host` selects only common policy without requiring a repository.
Information is passive; setters preserve cache-owned persistence/activation and
authority boundaries. [CLI Retention Commands](cli.md#cache-retention-commands)
owns grammar.

Service implementation is Rust-first: `wrix-service` owns shared-container
lifecycle, workspace registration, endpoint metadata, port leasing and
Dolt/proxy/cache hosting integration. Cache library/helpers and their immutable
Nix entry points belong to `cache.md`.

### Garbage Collection Scope

Acceptance: [GC Scope Acceptance](#gc-scope-acceptance).

`wrix service gc` collects Dolt garbage only in the selected canonical
workspace. It does not dispatch Celler collection or use that workspace as a
filter for a host-wide cache collector. Other workspaces, cache data and valid
proxy grants are unaffected; Celler availability is not a prerequisite for this
Dolt-only scope.

`wrix service gc --host` selects this host user's managed Dolt databases and one
host-wide native Celler collection cycle, independently of the current
repository. There is no per-workspace Celler maintenance path. It does not
target another host user's services or data. Age policy still protects unexpired
view references and native in-flight data; physical deduplication does not
authorize deleting those references. Collection is not namespace retirement,
shutdown, rotation or proxy-grant revocation and does not change host-management
authorization.

`wrix service gc --host` does not collect runtime images. `wrix service gc all`
combines that host-user-wide Dolt/Celler collection with the same
unused-Wrix-image collector as `wrix service gc image`, independently of the
current repository. The latter is image-only: it does not collect Dolt/Celler
data, register a workspace or start the shared service container. `all` does not
widen ownership, retention or protected-image eligibility, nor authorize other
users' stores, unrelated images, host Nix GC, volume removal or key deletion.
CLI validation precedes maintenance; the superseded `--image` flag and
`service host gc` / `service image gc` spellings are not compatibility surfaces.

The data-collection families are Dolt and Celler. Dolt collection uses native
full garbage collection through the already-running service against each
selected database. Full collection includes both old and new storage
generations, rather than collecting only the new generation; it does not broaden
workspace/database scope. It reclaims unreferenced storage objects without
deleting issue rows or reachable history; [Beads](beads.md) remains the
database/history owner. Only host/all scope invokes Celler collection. It uses
native age eligibility under
[Cache Eviction](cache.md#cache-commands-locking-and-retention) and
[Retention Control](cache.md#retention-control), without current-target pins or
root-aware deletion. Unexpired view/native in-flight references and valid
signing/issuer authority remain protected. Native batching can leave eligible
garbage after a successful cycle; storage errors/incomplete execution must not
be hidden as success or an exhaustive drain. Neither family authorizes host
`/nix/store` GC, whole-volume removal, credential/key deletion or raw-file
cleanup.

Runtime-image pruning has explicit `gc image` and combined `gc all` entry points
under [CLI Image Commands](cli.md#image-commands), with mechanics owned by
[Sandbox Image Cleanup](sandbox.md#image-retention-and-cleanup), not automatic
service startup. The former `wrix service dolt gc` only printed a database
location; it is retired, not a compatibility alias or a substitute for
maintenance.

### GC Availability

Acceptance: [GC Scope Acceptance](#gc-scope-acceptance).

The `gc`, `gc --host`, `gc image` and `gc all` commands do not automatically
start shared infrastructure or stopped backend components. They do not register
or reactivate namespaces, install service images, repair/restart/replace the
service or launch a substitute maintenance container merely to enable
collection. Ordinary development/launch flows and explicit service startup
retain their separate lifecycle contracts.

Data collection uses already-running, compatible and authorized backend access.
A required absent, stopped or otherwise unusable backend makes the affected
collection units unavailable/incomplete, not successfully collected. No data
collection is dispatched for a unit known to be unavailable. Missing service
availability is not evidence that managed data contains no eligible garbage, and
it does not authorize raw-file cleanup or an offline fallback.

`gc all` still attempts independently safe image collection when data services
are unavailable, provided the runtime image store is accessible. It reports the
unavailable data work alongside actual image results and returns nonzero under
[GC Failure Handling](#gc-failure-handling). Image-only GC does not depend on
Dolt/Celler availability. A positively established empty or inapplicable data
scope is a normal no-op without provisioning; unknown state must not be reported
as empty merely because a service cannot be contacted.

### GC Contention

Acceptance: [GC Scope Acceptance](#gc-scope-acceptance).

When another operation holds required maintenance access, GC waits within a
finite budget that defaults to 30 seconds for the affected collection unit. This
is a per-unit wait, not a whole-command deadline. Repeated acquisition attempts
do not reset that unit's deadline. If access becomes available within the
budget, collection may proceed after its normal safety checks. Otherwise the
unit is reported as incomplete and remaining independently safe work continues.

A parsed zero duration, such as `--wait 0s`, permits one immediate acquisition
attempt without waiting. Available units still collect after their safety
checks; contended units are incomplete, and other independently safe work
continues. Zero must not skip the initial attempt or suppress collection of free
units.

The caller may override this budget with `--wait <duration>` for one invocation;
all selected units use that invocation's budget. The override does not persist
or change a later command's 30-second default. No persistent GC setting is
introduced, and repository/cache-policy environment overrides do not select this
GC budget. Existing cache commands retain their own lock-timeout settings.
[CLI Service Scope](cli.md#service-scope) owns the option's parsing/help.

Contention never authorizes lock bypass/stealing or stopping peer work/services
to force progress. A contention timeout is incomplete requested work, not a
successful skip for an in-use/protected resource; final status follows
[GC Failure Handling](#gc-failure-handling). This budget bounds acquiring
maintenance access, not the duration of full collection after access is
obtained.

### GC Failure Handling

Acceptance: [GC Scope Acceptance](#gc-scope-acceptance).

After arguments and authorized scope are validated, all GC modes continue
remaining independently safe requested work after a collection-unit failure. A
failure in one workspace, backend or image does not suppress other units whose
authority, eligibility and backend consistency can still be established. Failed
checks never authorize deletion, bypass locks or weaken reference/retention
protection. If a shared prerequisite fails, dependent work cannot proceed, but
independent work can. Invalid arguments or an untrusted/ambiguous invocation
scope abort before deletion rather than being treated as ordinary unit failures.

Failures are accumulated and reported at the end alongside completed work,
including partial completion; the command must not report only the last error.
Any failed or incomplete requested collection work yields nonzero status. Having
no eligible garbage, or retaining resources because they are in use/unowned or
protected by policy, is not itself a failure. Completed reclamation is not
rolled back or described as transactional all-or-nothing cleanup.

This maintenance error policy does not change fail-fast supervision of the
shared service processes, permit disruptive service replacement or revoke valid
grants.

### Dolt service

Acceptance: [Host Service Acceptance](#host-service-acceptance).

Dolt runs inside the shared host-user service container alongside the egress
proxy, not as a host-native database daemon.

Bare `wrix service dolt` reports only the selected workspace's Dolt information:
enabled/configured status, available transport/endpoint metadata and a
connection command/template. It distinguishes configured endpoints from observed
service state; metadata alone is not authenticated SQL readiness. Absent/stopped
or disabled services can be described without provisioning or registration. The
summary never starts/repairs infrastructure, allocates ports, issues
credentials, changes database state or opens an interactive SQL client.

Connection output omits passwords, bearer tokens and private credential
material. The command/template correctly quotes endpoint arguments for supported
host shells, including paths with spaces or shell metacharacters. It uses only
the selected workspace's published client scope, never another tenant's endpoint
or shared SQL-administrator authority. Endpoint selectors remain useful for
scripts; status/attach are not retained as duplicate information commands.

Dolt access is bound to the selected workspace namespace, including database
selection, SQL authorization and file-remote access. A tenant-facing SQL
endpoint cannot expose another workspace or act as an unrestricted filesystem,
network or host-management deputy. A shared SQL administrator is not a sandbox
credential. The service preserves the logical transport distinction:

- Linux host and sandbox clients use the workspace Dolt Unix socket when
  available.
- Darwin clients use TCP because VirtioFS does not reliably carry Unix-socket
  operations.

When Darwin or another fallback needs Dolt TCP, the service binds only a
host-loopback port and publishes the exact endpoint through `services.json` /
`wrix service endpoints`. Linux does not publish a Dolt TCP port by default.

TCP startup provisions scoped workspace authorization for forwarded clients and
preserves database contents, unrelated users/grants and existing passwords.
Published endpoint and credential selection identify the workspace, not merely a
reachable shared server. Readiness requires an authenticated query in that
namespace; accepting TCP or authenticating against another tenant is
insufficient.

### Cache Endpoints

Acceptance: [Host Service Acceptance](#host-service-acceptance).

Cache authorization/endpoint separation exposes only the workspace's authorized
read set: its repository-private view and the common dependency view under
[Cache Visibility](cache.md#cache-visibility). A shared Celler listener/archive
store does not authorize unrelated private views. Published agent-facing
endpoints permit only that authorized read set. Host publishing/management uses
a separately authorized interface that is not a sandbox exception or proxy
destination. `cache.md` owns HTTP admission, runtime credentials, signing and
publication, including the
[Native Pull Credential Lifecycle](cache.md#native-pull-credentials).

The service publishes a host-loopback port
(`127.0.0.1:<cache-port> -> workspace cache listener`), persists the selected
endpoint in `services.json`, and launchers inject the resolved sandbox-visible
URL without relying on container DNS. Preferred host ports are deterministic
from the workspace hash: cache HTTP uses `21000–22999`, and Darwin/fallback Dolt
TCP uses `23000–24999`. If a preferred port is busy, wrix probes within the
matching range, persists the chosen port, and reuses it while available. Startup
fails if required host ports cannot be bound to loopback only.

Unexpected exit of an active proxy, Dolt, Celler or shared sccache daemon fails
the shared container rather than hiding an unavailable component. Intentional
namespace retirement is not a failure. This fail-fast physical boundary can
affect other workspaces; logical tenancy is not independent container-level
fault isolation.

## Success Criteria

### Compiler Service Acceptance

- The shared daemon uses one service-owned native LRU cache defaulting to `50G`;
  ordinary worker/repository/spawn settings cannot resize it, while host/private
  local caches remain independent and other disk data is not claimed as capped.
  [test?](../crates/wrix-service/tests/compiler_service.rs::shared_compiler_capacity_is_service_owned_and_defaults_to_fifty_g)

- Service planning/image integration hosts one actual sccache daemon alongside
  the other service families, without an extra compiler container/VM or HTTP
  blob-cache backend.
  [test?](../crates/wrix-service/tests/compiler_service.rs::shared_compiler_is_one_managed_cohosted_daemon)
- Compiler registration/request plans keep originating checkout, selected guest
  toolchain and writable outputs distinct, including concurrent clones of one
  canonical repository, without a common mutable Cargo target directory.
  [test?](../crates/wrix-service/tests/compiler_service.rs::compiler_requests_keep_origin_contexts_distinct)
- Compiler input/output admission preserves narrow host views and excludes
  personal caches, host store/daemon/control sockets and private
  issuer/publishing roots rather than solving paths with broad host mounts.
  [test?](../crates/wrix-service/tests/compiler_service.rs::compiler_registration_preserves_host_private_mount_boundary)
- Workspace retirement does not stop the host-user daemon, clear pooled data or
  interrupt peer requests; private/restricted launches register no compiler
  work.
  [test?](../crates/wrix-service/tests/compiler_service.rs::workspace_lifecycle_preserves_peer_compilation)
- Managed daemon idle behavior preserves caller-independent services; ordinary
  compile failures are job results, while unexpected daemon exit retains shared
  fail-fast behavior.
  [test?](../crates/wrix-service/tests/compiler_service.rs::compiler_lifetime_preserves_idle_and_fail_fast_contracts)

### Host Service Acceptance

- Bare Dolt information is scoped to the selected workspace and excludes
  passwords/tokens, private credential material and shared SQL-admin authority.
  [test?](../crates/wrix-service/tests/host_service.rs::dolt_information_is_scoped_and_secret_free)
- Dolt information describes enabled/disabled or absent/stopped state without
  provisioning, port allocation, credential issuance, database writes or
  interactive SQL-client execution; metadata alone never claims SQL readiness.
  [test?](../crates/wrix-service/tests/host_service.rs::dolt_information_is_read_only_and_honest_about_state)
- Printed connection commands/templates preserve endpoint arguments containing
  spaces or shell metacharacters without exposing secrets or widening scope.
  [test?](../crates/wrix-service/tests/host_service.rs::dolt_connection_commands_quote_arguments_without_secrets)

- Dolt endpoint payloads preserve Unix/TCP metadata shapes, and TCP endpoints
  carry a nonzero port rather than optional or sentinel port state
  [test](../crates/wrix-service/src/lifecycle/mod.rs::endpoint_payloads_serialize_without_missing_or_zero_tcp_ports)

- Concurrent starts across workspaces create at most one host-user service
  container and independently register each requested namespace
  [test?](../crates/wrix-cli/tests/host_service.rs::concurrent_workspaces_share_one_host_service)

- `wrix service dolt sandbox-endpoint` returns the running Apple service VM's
  non-loopback TCP host and internal SQL port, or fails if no guest-visible
  address exists; host publication remains loopback-only
  [test](../crates/wrix-cli/tests/service_lifecycle.rs::apple_sandbox_endpoint_uses_service_vm_instead_of_host_loopback)

- Workspace identity produces stable namespaces, port leases, workspace
  control/data roots and locks without a container identity per checkout
  [test?](../crates/wrix-service/tests/host_service.rs::workspace_namespaces_are_stable_and_distinct)

- `mkDevShell` starts the service container by default for the project cache,
  `nixCache = false` suppresses cache-only startup, and any service container
  survives the process that evaluated the shell hook
  [system](test-ci:test-services-devshell-start-independent)

- `wrix service start` delegates image installation/digest reuse to the shared
  installer using `image-builder.md` source metadata without broad image
  pruning, MRU-eight eviction or a separate service-specific installer
  [test?](../crates/wrix-cli/tests/service_cache.rs::service_start_uses_shared_image_digest_preflight_without_pruning)

- Cache-only service startup is suppressed for temp-directory scratch
  workspaces, so tests and integration runs do not create infrastructure solely
  for scratch cache use
  [test](../crates/wrix-cli/tests/service_cache.rs::temp_cache_only_start_never_creates_a_service_container)

- Loom bead clone paths under `.loom/beads/<id>` select the outer repository
  identity rather than a bead-specific workspace identity
  [test](../crates/wrix-service/tests/lifecycle.rs::loom_bead_workspace_uses_repo_service_identity)

- Loom's independent clone under `.loom/integration` selects the outer
  repository identity rather than an integration-specific workspace identity
  [test](../crates/wrix-service/tests/lifecycle.rs::loom_integration_workspace_uses_repo_service_identity)

- Linux beads clients reach Dolt through the workspace Unix socket, while Darwin
  beads clients receive the service container's TCP host/port endpoint
  [system](test-ci:test-services-dolt-platform-transport)

- TCP Dolt clients use scoped authorization for their selected workspace
  [test?](../crates/wrix-service/tests/host_service.rs::forwarded_sql_authorization_is_workspace_scoped)

- TCP startup repairs persisted localhost-only grants idempotently without
  losing database contents or unrelated users/grants
  [test](lifecycle::dolt::test::tcp_bootstrap_repairs_persisted_localhost_grants_without_losing_state)

- TCP startup does not reset an existing SQL login password
  [test](lifecycle::dolt::test::tcp_bootstrap_does_not_reset_an_existing_root_password)

- Workspace authentication initialization preserves the managed database and
  does not replace the shared container beneath unrelated users
  [test?](../crates/wrix-cli/tests/host_service.rs::tenant_auth_initialization_preserves_other_workspaces)

- TCP Dolt readiness rejects a reachable server that refuses SQL authentication
  [test](lifecycle::dolt::test::sql_probe_rejects_a_reachable_server_with_invalid_credentials)

- TCP Dolt readiness fails within a bounded startup budget when a listener
  accepts connections but never speaks SQL
  [test](../crates/wrix-cli/tests/service_lifecycle.rs::tcp_wait_rejects_a_listener_without_sql_authentication)

- The shared service starts its registered proxy/Dolt/cache components under
  intentional-stop-aware supervision
  [test?](../crates/wrix-cli/tests/host_service.rs::shared_components_have_intentional_stop_aware_supervision)

- Unexpected active-component exit fails the shared container; intentional
  retirement of one namespace does not terminate other namespaces
  [test?](../crates/wrix-service/tests/host_service.rs::unexpected_component_exit_fails_shared_service)

- Cache HTTP and fallback Dolt TCP host ports use their deterministic workspace
  ranges; busy-port fallback is persisted/reused while available, and inability
  to bind loopback-only causes startup failure.
  [test?](../crates/wrix-service/tests/host_service.rs::workspace_port_leases_are_scoped_persisted_and_loopback_only)

- Two independent workspaces share one host-user container without replacing it
  when a new workspace registers; their workspace control roots/endpoints remain
  distinct.
  [test?](../crates/wrix-cli/tests/host_service.rs::new_workspace_registers_without_container_replacement)
- Linux and Darwin managed Dolt/Celler data is narrowly mounted, including
  delegated cache-signing state, while host publishing/issuer/control roots,
  manifests and arbitrary host trees remain unmounted.
  [test?](../crates/wrix-cli/tests/host_service.rs::shared_mounts_separate_service_data_from_private_authority)
- Workspace information and stop/logs/endpoints select one workspace, preserving
  peer/common availability on stop. Information/stop with `--host` select shared
  infrastructure independently of repository context.
  [test?](../crates/wrix-cli/tests/host_service.rs::workspace_and_host_operations_have_distinct_scope)
- Workspace and host information do not register namespaces, create managed
  state, install images, start/repair services or issue grants when absent,
  stopped or running.
  [test?](../crates/wrix-service/tests/host_service.rs::scoped_information_never_provisions_or_mutates)
- Shell exit does not kill shared infrastructure, scratch cache-only
  registration is suppressed, and restricted launches ensure proxy availability
  even with Beads/cache disabled.
  [test?](../crates/wrix-cli/tests/host_service.rs::shared_startup_tracks_required_features_not_shell_lifetime)
- Invalid or concurrent registrations do not overwrite peer namespace state.
  [test?](../crates/wrix-service/tests/host_service.rs::namespace_registration_is_atomic_and_isolated)
- The packaged service image cohosts the proxy, Dolt and Celler through the
  existing image-source/install contract, without a host-native cache daemon.
  [system?](test-ci:test-services-host-image)
- Ordinary tenant SQL/file/cache/proxy interfaces deny access to peer SQL
  data/file remotes, unauthorized cache views and management authority, and
  cannot be used as unfiltered network/filesystem deputies. Delegated compiler
  execution follows the separate shared-service trust boundary.
  [system?](test-ci:test-services-host-tenancy)
- Cache endpoint publication includes only the private/common views authorized
  by host management, never implicit access to peer private caches.
  [test?](../crates/wrix-service/tests/host_service.rs::cache_endpoints_publish_only_host_authorized_views)
- Live sandbox service integration exposes through direct mounts and ordinary
  endpoint/credential delivery only authorized namespace/cache read scope and
  its narrow sync-worktree view, never all-workspace roots, Celler signing state
  or host publishing/management authority.
  [system?](test-ci:test-services-host-sandbox-isolation)

### GC Scope Acceptance

- Maintenance-access waiting defaults to 30 seconds per collection unit, not a
  whole-command or running-GC time limit.
  [test?](../crates/wrix-service/tests/host_service.rs::gc_access_wait_defaults_to_thirty_seconds)
- An explicit `--wait` supplies each selected unit's acquisition budget only for
  that invocation; later invocations use the default, unaffected by prior runs
  or workspace/cache-policy overrides.
  [test?](../crates/wrix-service/tests/host_service.rs::gc_wait_override_is_per_invocation_not_workspace_policy)
- Zero wait attempts maintenance access once immediately: available units still
  collect normally, while busy units are incomplete without waiting and other
  independently safe units continue.
  [test?](../crates/wrix-service/tests/host_service.rs::gc_zero_wait_attempts_once_without_skipping_free_units)
- Busy required maintenance access uses one finite waiting budget per collection
  unit; retries do not restart the deadline, and timely acquisition permits
  normal collection after safety checks.
  [test?](../crates/wrix-service/tests/host_service.rs::gc_waits_for_access_with_one_finite_unit_budget)
- Contention timeout reports the affected unit incomplete, without lock bypass,
  lock stealing or stopping peer work/services, and independently safe work
  continues under the final nonzero failure policy.
  [test?](../crates/wrix-service/tests/host_service.rs::gc_access_timeout_preserves_locks_and_continues_safe_work)
- GC does not start/provision stopped or absent infrastructure, reactivate
  namespaces, install service images, repair/replace services or launch a
  substitute maintenance container to make collection possible.
  [test?](../crates/wrix-service/tests/host_service.rs::gc_does_not_start_or_prepare_unavailable_services)
- Required data units with unavailable backends are reported as incomplete,
  without dispatching their collection or treating unknown state as empty.
  [test?](../crates/wrix-service/tests/host_service.rs::gc_reports_unavailable_data_units_without_collecting)
- With unavailable data services and an accessible image runtime, `gc all` still
  attempts safe image collection and returns nonzero with both kinds of results.
  [test?](../crates/wrix-service/tests/host_service.rs::gc_all_still_attempts_images_when_data_services_unavailable)
- A positively established empty/inapplicable data scope succeeds as a no-op
  without provisioning infrastructure.
  [test?](../crates/wrix-service/tests/host_service.rs::gc_known_empty_scope_is_noop_without_provisioning)
- A collection-unit failure does not suppress remaining independently safe
  requested work across workspaces/backends/images, including combined GC.
  [test?](../crates/wrix-service/tests/host_service.rs::gc_continues_independently_safe_units_after_failure)
- Failed checks/shared prerequisites prevent affected work without authorizing
  unsafe deletion or disabling independent units; existing service supervision
  and valid grants are not weakened to continue GC.
  [test?](../crates/wrix-service/tests/host_service.rs::gc_failures_preserve_safety_and_dependency_boundaries)
- Invalid arguments or untrusted/ambiguous invocation scope prevent all deletion
  rather than allowing a partial run against guessed targets.
  [test?](../crates/wrix-service/tests/host_service.rs::gc_invalid_invocation_scope_prevents_deletion)
- Final GC reporting includes all observed failures and completed work without
  rollback promises or hiding earlier failures behind the last error.
  [test?](../crates/wrix-cli/tests/host_service.rs::gc_reports_completed_work_and_all_failures_at_end)
- GC returns nonzero for failed/incomplete work and distinguishes ordinary
  protected/empty collection from failure.
  [test?](../crates/wrix-cli/tests/host_service.rs::gc_exit_status_reflects_failed_or_incomplete_work)
- Host GC collects only data; `gc all` combines host-user-wide data collection
  with the same protected Wrix-image collector as image-only GC, independent of
  cwd and without widening retention, ownership or deletion authority.
  [test?](../crates/wrix-service/tests/host_service.rs::gc_all_combines_host_data_and_same_image_collector)
- Dolt collection selects the authorized workspace database and requests its
  native full collection through the already-running service, not
  default/shallow collection or direct filesystem maintenance.
  [test?](../crates/wrix-service/tests/host_service.rs::dolt_gc_requests_full_collection_in_selected_database)
- Real scoped Dolt full collection reclaims unreferenced objects from both old
  and new generations while keeping issue data and reachable history usable.
  [system?](test-ci:test-services-gc-data)
- Real host-wide native Celler collection expires eligible private/common
  entries even for current targets, preserves unexpired/native in-flight data
  and valid signing keys, and does not remove host store paths, volumes or
  credentials. [system?](test-ci:test-services-gc-data)
- Workspace GC selects only its canonical Dolt database, including Loom's outer
  repository identity, without collecting peer data or dispatching/probing
  Celler.
  [test?](../crates/wrix-service/tests/host_service.rs::workspace_gc_is_dolt_only_and_namespace_scoped)
- Host/all GC invokes one host-user-wide native Celler cycle, not per-workspace
  filtering or repeated cache collection for each database.
  [test?](../crates/wrix-service/tests/host_service.rs::celler_gc_runs_once_only_in_host_or_all_scope)
- Celler storage/collection errors and incomplete native execution yield failed
  work even when a backend entry point would otherwise hide an internal error;
  completed batches are not reported as an exhaustive drain.
  [test?](../crates/wrix-service/tests/host_service.rs::native_celler_gc_outcomes_are_truthful_and_batch_bounded)
- Host GC enumerates this host user's managed namespaces independently of the
  current repository, never another user's managed service roots.
  [test?](../crates/wrix-service/tests/host_service.rs::host_gc_is_repository_independent_and_host_user_scoped)
- GC preserves unexpired shared-view/native in-flight data and valid proxy
  grants instead of treating deduplication, shutdown or rotation as deletion
  authority.
  [test?](../crates/wrix-service/tests/host_service.rs::gc_preserves_unexpired_inflight_data_and_valid_grants)

### Compatibility Acceptance

- Compatible running host infrastructure is reused without runtime removal,
  recreation or image installation.
  [test?](../crates/wrix-cli/tests/host_service.rs::compatible_shared_service_is_reused)
- Service start and both launch modes reject incompatible/unverifiable running
  infrastructure without changing managed data, endpoints or peer proxy grants.
  [test?](../crates/wrix-cli/tests/host_service.rs::workspace_start_rejects_incompatible_service_without_mutation)
- Packaged devshell startup propagates incompatibility instead of silently
  replacing shared infrastructure.
  [system?](test-ci:test-services-nondisruptive-startup)
- Explicit host stop followed by startup may create the selected service while
  preserving managed databases, Celler cache data/valid signing keys and
  host-private publishing/management authority.
  [test?](../crates/wrix-cli/tests/host_service.rs::explicit_host_replacement_preserves_managed_workspace_state)
- Missing or fully stopped services can initialize normally; stopping only a
  workspace does not authorize replacement of an incompatible running service.
  [test?](../crates/wrix-cli/tests/host_service.rs::workspace_stop_is_not_host_replacement_authorization)

## Requirements

Acceptance: [Host Service Acceptance](#host-service-acceptance),
[GC Scope Acceptance](#gc-scope-acceptance),
[Compiler Service Acceptance](#compiler-service-acceptance) and
[Compatibility Acceptance](#compatibility-acceptance).

### Functional

1. **Host and workspace identity** — one container belongs to the host user;
   namespaces follow [Architecture](#architecture) without data/access/port/lock
   collisions.
2. **Service operations** — lifecycle and Dolt operations follow
   [Service Operations](#service-operations); explicit collection scope follows
   [Garbage Collection Scope](#garbage-collection-scope),
   [GC Availability](#gc-availability) and [GC Contention](#gc-contention).
   Cache semantics belong to `cache.md`.
3. **Lifecycle management** — shared creation, namespace registration, read-only
   information and scoped stop follow [Lifecycle](#lifecycle), independently of
   caller lifetime.
4. **Compatibility and replacement** — reuse, failure and explicit host
   replacement follow
   [Compatibility and Replacement](#compatibility-and-replacement).
5. **Service image source** — creation uses the shared installer described in
   [Service Image Source](#service-image-source), not a service-specific path.
6. **Endpoint publication** — selected-workspace SQL and cache access follow
   [Dolt Service](#dolt-service) and [Cache Endpoints](#cache-endpoints).
7. **Shared compiler** — admitted job context, the single native cache budget,
   lifecycle and trust boundary follow
   [Shared Compiler Service](#shared-compiler-service).

### Non-Functional

1. **Logical project isolation** — host-private authority, delegated Celler
   signing state and service-visible tenant views follow
   [State Layout](#state-layout); cohosting is not independent container-level
   compromise/fault isolation.
2. **Host-store isolation** — no broad host filesystem, store or daemon mounts
   follow [State Layout](#state-layout).
3. **Caller independence** — infrastructure persists until explicit host stop,
   as defined by [Lifecycle](#lifecycle).
4. **Fail-fast supervision** — unexpected component failure versus intentional
   namespace retirement follows [Cache Endpoints](#cache-endpoints).

## Out of Scope

- An invasive sccache fork or replacement of its native compiler/cache
  semantics, a shared worker-runtime migration, or a custom dynamic mount broker
  solely to make compiler delegation work. Thin transport/path adapters are
  allowed within the existing host/worker/service boundaries; inability to
  satisfy those boundaries calls for a new design decision, not silent scope
  expansion.

- Machine-wide multi-user hosting, unpartitioned global Beads trust, implicit
  union of private cache views/keys, and broad host-home or repository-tree
  mounts. The approved common dependency audience belongs to `cache.md`.
- Per-workspace Celler collection and sharing-group management/publication;
  cache eviction is native and manual collection is host-wide.
- Moving full worker runtimes into this service container or adopting a shared
  containerd/nix-snapshotter execution appliance. That runtime migration is
  deferred; explicitly delegated compiler jobs are the separate agreed
  exception.
- Independent container-level compromise/fault isolation between cohosted
  service components or workspace tenants.
- Host-native Dolt and host OS account/module provisioning for a native Dolt
  daemon. Moving only Dolt does not eliminate the proxy's service container and
  would add a separate privileged setup and lifecycle.
- Automatic replacement/upgrade of a running host service from an ordinary
  workspace startup; explicit host shutdown is required first.
- Automatic idle shutdown, background execution reconciliation or recovery of
  agent work; host-wide shutdown is explicit.
- Cache contents, transport/signing/publication/retention and cache exclusions
  beyond hosting/namespace/endpoint duties; those belong to `cache.md`.
