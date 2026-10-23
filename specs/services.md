# Host Services

One managed service deployment per host user, hosting logically isolated
workspace Dolt and project Nix caches, a shared compiler daemon and per-launch
egress policies, with replaceable container generations.

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
service-deployment identity per host user and the operations behind
`wrix service ...`; it is not a machine-wide multi-user service. Physical
container ownership is independent of repository paths. Workspace namespaces
remain keyed by canonical repository identity, with Loom bead/integration clones
using the outer repository identity and non-repository paths using their
canonical path. Namespace authorization, state and locks remain distinct;
transport listeners belong to deployment roles rather than individual
workspaces. Repository cache trust remains private except for the common
dependency view explicitly authorized by
[Cache Visibility](cache.md#cache-visibility).

Acceptance: [Host Service Acceptance](#host-service-acceptance) and
[Compiler Service Acceptance](#compiler-service-acceptance),
[Generation Acceptance](#generation-acceptance) and
[Backend Upgrade Acceptance](#backend-upgrade-acceptance).

Normally one selected generation serves the deployment. Explicit updates may run
separate Blue/Green generation containers concurrently; workspace identity is
not a container-generation identity. Roles can hand over independently while
other work remains on the older generation. Each stateful backend still has only
one active owner. Container separation does not introduce compiler-only
isolation or a stronger tenant compromise guarantee.

The deployment owns four core service families:

- **Dolt for beads** — the Dolt SQL server for the Beads-owned workspace
  database. `beads.md` owns issue tracking and database storage; this spec owns
  only deployment/generation lifecycle and endpoint publication.
- **Project Nix cache** — Celler runs inside managed service containers, with
  scoped read-only client views and host-authorized publishing. `cache.md` owns
  cache contents, transport, signing, publication and retention; this spec owns
  placement, namespace isolation and endpoints.

- **Egress proxy** — shared HTTP/CONNECT infrastructure with isolated per-launch
  grants. `proxy.md` owns request admission, authentication and network safety;
  this spec owns its placement and service lifecycle.
- **Shared compiler** — one actual sccache daemon for eligible shared Rust
  launches under [Shared Compiler Service](#shared-compiler-service), not a
  generic HTTP blob-cache backend or full worker-runtime migration.

### Service-Owned Nix Stores

[Service-Owned Nix Stores](store.md) owns the additional pool-scoped backend
family, private database/build state, untrusted clients, read-only worker views
and native GC. Pools use this deployment/controller and single-owner lifecycle;
they are not new per-workspace service deployments. Their own selection and
approval do not enable Celler, compiler or proxy grants. Pool state is separate
from Celler archives and compiler-cache data. Acceptance:
[Store Pools](store.md#pool-acceptance) and
[Store Lifecycle](store.md#lifecycle-acceptance).

### Host Control Service

Acceptance: [Host Control Acceptance](#host-control-acceptance).

One small host-user Wrix controller outside replaceable service containers owns
deployment selection, workspace registration, host-controlled authorization,
credential issuance, stable role routing and eventual generation retirement. It
runs without root or machine-wide multi-user authority. CLI/shell clients
request typed operations, not independent detached forwarders or cleanup
helpers. Private issuer/management credentials and authoritative
ownership/policy records stay host-side. The authenticated local control
interface admits no arbitrary commands, paths, mounts or forwarding
destinations; neither its socket nor its credentials reach guests. Container
reports and workspace pointers cannot grant host authority or alone establish
backend ownership release.

Rama supplies bounded fixed-role transport forwarding, not host-side SQL
parsing, protocol sniffing or a general destination proxy. Egress HTTP/CONNECT
policy remains in the replaceable gateway under `proxy.md`; Celler retains
native cache authorization. Existing connections pin their admitted route while
new connections follow selection. Native backend work/drain accounting is
separate from transport connection counts.

The controller does not supervise agents, broker provider/Git credentials,
execute general Nix/Cargo commands, serve the host store, or replace native
cache storage/GC. Waiting launchers retain execution observation and sandbox
credential renewal. Host publishers retain selected-graph discovery, verified
public/private classification and NAR streaming.

### Controller Lifecycle and Recovery

Acceptance: [Host Control Acceptance](#host-control-acceptance).

Platform-supported Linux/Darwin user supervision gives one controller
caller-independent lifetime, including explicitly temporary Nix provision.
Concurrent start is serialized; explicit host stop prevents supervisor-driven
resurrection. Information, preflight and GC never bootstrap or repair it. Active
controller/image/runtime closures remain Nix-GC-reachable, without required root
installation or a particular profile path.

Clients admit the versioned private control interface and required capabilities
before effects. Package installation does not replace an active controller.
Compatible backend updates preserve it. Incompatible running-controller
replacement requires explicit maintenance, never ordinary startup. The retained
cached-policy epoch upgrade has an implemented `wrix service repair` operation:
it verifies the recorded native unit, stops only that controller, holds the
original ownership inode while migrating policy, selects the updated immutable
controller closure, and authenticates readiness before backend recovery. It
retains namespace policies and management/issuer keys; foreign or uncertain
units and other ownership holders fail closed. Ordinary `service recover` checks
this compatibility before creating or starting backend containers.
[test](../crates/wrix-cli/tests/service_lifecycle.rs::repair_upgrades_cached_controller_and_recovers_backend_with_native_authenticated_readiness)
[test](../crates/wrix-cli/tests/service_lifecycle.rs::recovery_checks_legacy_controller_before_starting_or_creating_backend)

Whole-deployment handoff follows `wrix service stop --host` then
`wrix service update`. Updating that stopped existing deployment selects the
invoked package's controller and bound service image; ordinary start resumes
recorded controller/backend versions, not the caller's newer package. Controller
replacement/restart is not transparently stream-preserving.

Durable state records role versions, generation identities and ownership
transition phases, not live proxy grants. After failure, uncertain ownership or
readiness keeps affected admission closed until observed runtime/backend state
is reconciled. Never open a second owner, infer completed native work from a
lost socket, revive saved grants, replay SQL/compiler work or fabricate agent
completion. Recovery promises safe ownership and truthful partial status, not
uninterrupted service or arbitrary power-loss durability.

### Shared Role Endpoints

Acceptance: [Host Control Acceptance](#host-control-acceptance) and
[Host Service Acceptance](#host-service-acceptance).

Use one bounded deployment-level endpoint set for SQL, cache reads, egress and
eligible compiler access, with separate host-only management/publication routes.
New workspace registration provisions scoped credentials/database/cache
membership, not workspace TCP ports, serving-mount changes or new containers.
Linux may retain narrow namespace Unix-socket aliases. Each generation's role
listeners are established at creation through supported runtime mechanisms; the
controller changes fixed authorized destinations, not live runtime publication.

Host publication remains loopback-only. Backend-specific guest routes must be
proven reachable without LAN/wildcard fallback, host firewall mutation, assumed
cross-VM Unix sockets or treating host loopback as directly guest-reachable.
Shared SQL requires native scoped database/file-remote authorization and denial
of filesystem/network deputy operations; Rama cannot manufacture SQL tenancy. If
safe tenancy or guest routing is infeasible, return for a concrete design
decision, not broader mounts or per-workspace deployments. Source investigation,
native Rust tests and interface implementation proceed without Mac access;
actual Podman/krun/Apple proof gates integration, not all foundational work.

### Shared Compiler Service

Acceptance: [Compiler Service Acceptance](#compiler-service-acceptance).

The actual shared sccache daemon runs with the other backend service families,
with service-owned persistent compiler-cache data. Generation overlap does not
introduce a second active compiler-cache owner or a compiler-only isolation
container/VM. It does not use personal-host sccache data or another HTTP cache
backend. Gateway/backend roles may temporarily span generations during the
controlled handoffs below.
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
allowing one workspace to replace serving generations. Private/restricted
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

Acceptance: [Host Service Acceptance](#host-service-acceptance) and
[Generation Acceptance](#generation-acceptance).

Host-private management, publishing and credential-issuer state is separate from
service-visible workspace data and Celler's delegated cache-signing state.
Workspace-private state roots remain platform-native:

| Platform | Durable state root                                                        |
| -------- | ------------------------------------------------------------------------- |
| Linux    | `${XDG_STATE_HOME:-$HOME/.local/state}/wrix/workspaces/<workspace-hash>/` |
| Darwin   | `$HOME/Library/Application Support/wrix/workspaces/<workspace-hash>/`     |

Host namespace endpoint references and their schema version live in the
native-root `state.db`, not a per-workspace `services.json` copy. The
`wrix service endpoints` interface and worker projections render the required
endpoint observations without exposing the database. Host metadata authority and
explicit native import follow
[Workspace Control Storage](workspaces.md#native-control-storage). Host-managed
deployment state also records selected generation/version, serving/draining
roles and ownership transitions; it is not an agent-controlled selection or a
durable store of live traffic grants. Stopping a generation retains its
selection and version. `cache.md` owns host-private namespace
publication/pending/status state, public-key metadata and the separately managed
Celler signing/archive data.

Service-visible workspace data roots are:

| Platform | Workspace service-data root                                                            |
| -------- | -------------------------------------------------------------------------------------- |
| Linux    | `${XDG_STATE_HOME:-$HOME/.local/state}/wrix/service-data/workspaces/<workspace-hash>/` |
| Darwin   | `$HOME/Library/Application Support/wrix/service-data/workspaces/<workspace-hash>/`     |

`beads.md` owns the live database and canonical sync-worktree children beneath
this root. Dolt, Celler metadata/signing/archive state and shared compiler-cache
data occupy separate managed persistent areas, independent of generation
lifetimes. They are not one freely writable volume mounted into every color.
Each backend has one active owner; preparing another generation does not open
its databases or acquire concurrent cache ownership. Runtime volume attachment
and native filesystem behavior must satisfy this on both backends, not assume
Apple permits parallel attachment of a named volume.

Service generations mount only admitted managed data and compiler input/output
views, not arbitrary repository trees or the host home. Celler's managed mounts
permit its own metadata/signing/archive writes. New workspaces register without
replacing serving generations or changing their mount sets. Tenant-facing APIs
expose only authorized workspace data and endpoints; a shared parent mount is
not permission for clients to traverse other tenants.

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

Acceptance: [Host Service Acceptance](#host-service-acceptance) and
[Generation Acceptance](#generation-acceptance).

`mkDevShell` ensures the shared service and registers the selected namespace
when its project cache is enabled or Beads is configured. Nix-cache opt-out does
not disable Beads, proxy or shared compiler use. Both sandbox launch modes
ensure required namespace services; shared Rust also requires its compiler
service even when the Nix cache/Beads are off. `limit` requires the shared
proxy, with Rust using private tool caches rather than shared compiler
delegation. Registration, generation selection and creation are concurrency-safe
and idempotent; concurrent ordinary starts do not create competing deployments.
A malformed registration cannot overwrite another workspace's state or policy.

Service lifetime is caller-independent: shell/editor exit does not kill serving
or draining generations. Ordinary `wrix service stop` retires only the selected
workspace's services and proxy grants, leaving other workspaces and shared
common dependency view available. `wrix service stop --host` is the explicit
host-user-wide shutdown of all serving/draining generations and revokes all
service availability and proxy grants. It preserves data and the selected
generation/version; later startup resumes that selection rather than implicitly
selecting a newer image. Resume also respects recorded backend role versions: a
selected Green gateway does not authorize silently migrating a backend still
assigned to Blue after a partial update. Bare `wrix service` reports the
selected workspace; `wrix service --host` reports deployment/generation state
without requiring a repository. Both information defaults are read-only even
when infrastructure is absent/stopped: no namespace registration, managed-state
creation, image installation, service startup/repair or grant issuance. Logs and
endpoint queries remain workspace-scoped. Host operations select the current
user's shared infrastructure, not another machine or elevated authority. No
automatic idle timer is introduced. A stopped selected gateway remains selected
and is restarted at that version when required; absence of traffic is not
permission to retire the last selected generation. Stopping an in-use gateway is
disruptive shutdown, not transparent resource saving.

Cache-only registration remains suppressed for temporary scratch workspaces.
External clones share a native namespace only through
[explicit workspace admission](workspaces.md), not directory naming. A requested
restricted launch may still use shared proxy infrastructure in a scratch
workspace. Proxy startup/registration failure prevents restricted agent launch;
later proxy failure never enables direct egress.

### Compatibility and Replacement

Acceptance: [Compatibility Acceptance](#compatibility-acceptance) and
[Generation Acceptance](#generation-acceptance).

Ordinary workspace startup (`mkDevShell`, `run`, `spawn` and
`wrix service start`) reuses a compatible running host service. Compatibility
must be positively established for the required service interfaces and policy
features; unknown or unverifiable compatibility is not permission to replace it.

An incompatible running service causes nonzero failure with explicit host-wide
remediation. Startup does not remove/recreate it, install a replacement image,
alter managed database/cache data, rewrite endpoints or revoke peer proxy
grants. A workspace stop does not authorize host replacement. Explicit
deployment shutdown remains `wrix service stop --host`, with ordinary
`wrix service start` resuming the selected version. That shutdown is
deliberately disruptive to all namespaces, but retains managed workspace data,
valid Celler signing state and host-private publishing/management authority. Old
traffic grants do not survive shutdown/restart. Changing the selected generation
is a separate host-authorized activation under
[Generation Handoff](#generation-handoff), not an implicit effect of stop/start.
Separately authorized Dolt backend maintenance follows
[Dolt Backend Handoff](#dolt-backend-handoff); it is not permission for ordinary
startup to replace shared infrastructure.

Creating a missing or fully stopped service is normal startup, not replacement
of a running service. Read-only launch preflight reports a required incompatible
service as a blocker without repair; a stopped service remains a preparation
need. Compatibility is rechecked by actual startup, not authorized by a prior
preview.

### Generation Handoff

Acceptance: [Generation Acceptance](#generation-acceptance).

Blue and Green are separate managed container generations, not separate
workspace tenants or concurrent writable copies of backend data. A replacement
is prepared without touching the live databases or activating another compiler
cache owner. Only explicit host-authorized `wrix service update` changes an
existing selected generation under
[CLI Service Update](cli.md#service-update-command). Its scope is this host
user's deployment, not the current workspace, with no redundant `--host` flag;
cache/proxy traffic credentials are not update authority. Its candidate comes
from the explicit service-image binding supplied by the invoked host Wrix
package through the existing image-source contract. The
[host CLI/project-data workflow](cli.md#host-cli-and-project-configuration)
keeps this package on PATH while projects supply launch configuration, not a
second executable or service-image override. Home Manager, nix-darwin, user
profiles and explicitly temporary Nix provision are installation choices, not
different image-selection rules.

The binding, not an executable/store-path hash, version string, current
repository, launch configuration or previously selected image, identifies the
candidate. Packages can share a Rust executable while supplying different
service-image definitions; use actual managed image identity for compatible
target reuse. Ambient/project image defaults and latest-release lookup cannot
replace that binding. Resolve and validate it before live selection changes;
missing input does not authorize guessing or repository discovery. Service
operations do not require launcher-only `--profile-config`.

The update plan always includes all four core deployment roles: gateway, Dolt,
Celler and shared sccache. Provisioned Nix pools add the ownership, drain and
readiness obligations in
[Store Lifecycle](store.md#bootstrap-protection-and-lifecycle); core-role
readiness alone cannot complete those transitions. Idle or stopped roles and
roles unused by the current repository are not omitted, and there is no per-role
update selector. Complete activation requires confirmed readiness at the target
for all four roles, not just a new gateway or the presence of their binaries in
an image. A failed or incomplete role remains required even when it currently
has no traffic. Updating these service roles does not enable cache access,
publication or shared tools for workspaces whose policies disable them, nor
broaden their permissions.

Update requires an existing deployment and fails before creation when none
exists, directing the user to ordinary `wrix service start` from a repository.
For an existing stopped deployment, explicit update starts and activates the
candidate using the normal ownership, readiness and recovery requirements. It
does not only record a staged version for later validation. Ordinary start
continues to resume recorded gateway/backend versions without implicit update.

Concurrent updates must not race selection or create competing backend owners.
Repeating an already-applied compatible target does not churn the generation or
revoke grants; already-pending drain/retirement can still progress. Ordinary
launch/start and actual or cached direnv activation reuse or resume the
selection, report incompatibility/update needs, and do not silently activate a
candidate. Information and preflight remain read-only.

Sandbox-facing endpoints remain stable across a planned handoff. For egress
proxy traffic, trusted connection-aware routing sends newly established
connections to the selected gateway while existing proxy connections retain
their old paths. This is not a DNS change alone, a rewrite of agent
environments, live TCP migration or a request that users reload/restart their
sandboxes. Existing keep-alive connections still receive ordinary per-request
admission. Internal forwarding reaches only fixed authorized service listeners,
never arbitrary destinations or management APIs. The stable routing/control
boundary is outside the replaceable gateway selection; its own replacement is
not implicitly interruption-free.

For proxy traffic, complete the authenticated active-grant handoff in
[Proxy Planned Handoff](proxy.md#planned-gateway-handoff) before new routing
requires Green to admit those launches. Preserve immutable policy, scoped
credentials and a live launcher owner; unknown, revoked or failed transfers are
not authorization. A failed preparation cannot silently reroute clients to an
unready gateway. Report any partial ownership/routing effects truthfully rather
than claiming an atomic whole-deployment rollback.

An existing egress download or opaque CONNECT tunnel remains on Blue until its
connection closes; a later proxy connection from that same sandbox uses Green.
Transferring launch ownership means a long-lived sandbox alone does not pin
Blue. Such long-lived or idle proxy connections can retain it indefinitely:
normal update does not force-close them to meet a retirement deadline. The
separate explicit host shutdown remains disruptive, not a hidden force-update
mode.

After successful selection, Blue becomes eligible for managed removal only when
its protected egress connections and SQL sessions have finished, and its
ownership/coordination obligations, backend roles and background/in-flight work
have finished or been safely handed over. Idle Celler/sccache transports follow
[backend work-drain rules](#cache-and-compiler-backend-handoff), not a
requirement that every idle backend socket close naturally. Selected Green must
already be ready for the roles it has taken over. Report selection, remaining
old-generation work and retirement separately; successful routing activation
does not mean old streams migrated or the old container vanished. Once all core
role transitions and provisioned store obligations are ready, activation may
succeed while protected proxy connections still drain; that is not a failed
required transition or a wait for whole sandbox lifetimes. Failed/incomplete
required role transitions remain nonzero with known partial state under
[CLI Update Results](cli.md#service-update-command). Retirement removes obsolete
runtime containers, not persistent service data or cached images outside
explicit image GC. Never retire the last selected gateway merely because it is
idle or stopped. If it is stopped without replacement, resume that recorded
generation/version, not an arbitrary latest image. If that version cannot be
restored safely, fail actionably rather than silently selecting a different one.

Once those removal conditions hold, retire the obsolete generation
**automatically**, even after the successful update command has returned. The
managed service/runtime lifecycle must retain cleanup ownership beyond that
client process; another update, GC, start or other Wrix invocation is not
required. This is generation cleanup, not agent execution reconciliation, and
must not give workers or cohosted compiler code host runtime-control authority.
Uncertain eligibility cannot authorize removal. Failed removal remains recorded
as incomplete/failed retirement rather than falsely reported as completed or
worked around by closing protected connections or interrupting unfinished work;
readiness of the selected replacement does not waive any drain or ownership
guard. Automatic retirement adds no forced deadline for protected connections
that remain live.

Stable routing and data handoff must be demonstrated with actual Podman and
Apple paths, including their TCP/Unix, authentication, mount and shutdown
behavior. The host controller owns stable Rama routing; neither DNS changes nor
an untested volume/network assumption is sufficient. No broader host mounts,
worker runtime-control socket or full worker runtime migration is authorized to
make this work.

### Cache and Compiler Backend Handoff

Acceptance: [Backend Upgrade Acceptance](#backend-upgrade-acceptance).

Backend version changes are separately coordinated single-owner maintenance, not
a side effect of switching proxy routes. Dolt follows
[Dolt Backend Handoff](#dolt-backend-handoff). Celler and sccache use the same
prepare, close-new-admission, drain, clean-release, new-owner-readiness pattern.
Use finite busy-drain budgets; failure to establish quiescence aborts that
backend transition before update-driven stop/data changes and restores its old
admission. These are not the GC acquisition budget or a force-kill deadline.

Celler drains admitted HTTP transfers and whole multi-request publication
workflows, and completes or safely quiesces background maintenance and metadata
writes before release. An active publication remains admitted work between HTTP
requests; a quiet socket or empty request count is not proof of completion.
Transfer the existing metadata, signing state, archives and effective policy to
one new owner, without simultaneous native database opens, key rotation or
view/retention reset. Authenticate readiness through actual scoped HTTP access,
not just an open port. Incomplete multi-request publication remains failed or
pending under cache caller rules; do not report an upload complete or silently
replay an entire publication because one request finished.

Sccache drains native compiler jobs, originating output delivery and pending
cache writes before stopping its old owner. Preserve the one native 50G cache
budget and correct source/toolchain/cwd/output mappings; never start a second
server on the same local cache, kill a job to satisfy an update, or replay a
compile across generations. Validate new-owner native protocol/toolchain
readiness before admitting new work. Existing compatible cache entries remain
reusable; this does not make evictable cache artifacts a permanent archive.

For Celler and sccache the drain unit is admitted backend work, not every
transport connection. Once native completion/quiescence is proven and admission
coordination prevents new work racing that decision, clean shutdown may close
truly idle backend transports; clients may need to reconnect to the same stable
endpoint. No age-based idle guess, socket count or uncertain native state is
proof that publication, compiler output or metadata/cache writes have finished.
Uncertainty or busy timeout aborts before stopping the old backend rather than
killing admitted work. This permission does not apply to protected egress
HTTP/CONNECT connections or whole Dolt sessions, including idle SQL sessions.

New backend operations may be briefly unavailable during the ownership gap;
there is no universal zero-downtime or transparent retry guarantee. A blocked
backend can retain an old-generation role even after proxy routing changes.
Report completed, draining, aborted and failed transitions distinctly, return
nonzero for failed required transitions, and do not tear down their old
container to force completion. After ownership release or migration, unsafe
old-binary rollback is forbidden; preserve recovery state and report unready
roles rather than inventing success. Planned role stops are not unexpected
process deaths and do not stop unrelated roles or revoke proxy grants.

### Dolt Backend Handoff

Acceptance: [Dolt Handoff Acceptance](#dolt-handoff-acceptance).

A controlled Dolt version handoff is host-authorized maintenance, distinct from
whole-container shutdown. Ordinary startup, information, preflight and GC do not
initiate it or bypass its exclusive maintenance ownership. Preparing the
candidate image/configuration does not open the live database, run migrations or
claim production readiness. Persistent database data is independent of the
selected version; handoff retains one authoritative copy and one live owner
rather than using simultaneous read-only/read-write servers or mount-role flips.

Before stopping the current backend, close new SQL admission for every namespace
it serves, across all live gateway generations and TCP/Unix or maintenance entry
points. Close admission atomically with admitted-connection accounting so an
in-flight connection cannot race an observed drained state. New client
connections fail promptly without reaching the backend; they are not queued and
no SQL is buffered or replayed. Native SQL clients receive a connection failure,
not an HTTP response masquerading as a MySQL error.

Existing sessions remain attached to the old backend and may finish normally.
Drain waits for whole sessions, including idle or transaction-owning sessions,
not merely the absence of executing statements. Backend-side completion must be
established as well as gateway-side closure; an inaccessible backend or a
vanished downstream socket is not proof of quiescence. Native inspection must
cover all relevant sessions, excluding only identified maintenance-observer
connections, and maintenance work cannot bypass the closed admission boundary.

Drain uses a finite budget. If quiescence cannot be established within that
budget, abort the update before stopping the old backend or changing its
version/configuration/database format, reopen its SQL admissions and report
nonzero. Do not force-close existing sessions, steal locks or use runtime
forced-kill escalation to make the update succeed.

Only after quiescence may the old backend shut down cleanly. Verify completion
and storage-ownership release before a new version opens the same persistent
data. Establish a consistent recovery checkpoint before a potentially
incompatible migration. The new owner must pass authenticated readiness for the
affected namespaces before client admission reopens. A socket accepting TCP is
not sufficient readiness. Preserve database/history contents and existing
workspace authorization; the handoff does not reinitialize databases or rotate
credentials merely to obtain readiness.

Failures after shutdown or migration are not a successful pre-stop abort. Keep
admission closed when ownership, data compatibility or readiness is unknown,
report the reached phase and required recovery, and never start an old binary
against migrated data without established compatibility or a valid recovery
procedure. A disconnected write can have an uncertain outcome; do not silently
retry SQL or claim transaction migration.

Intentional Dolt maintenance does not stop workers, revoke launcher-owned proxy
grants or interrupt unrelated proxy/cache/compiler work. It is an expected
component lifecycle transition, not an unexpected-exit trigger for the entire
service. Unexpected failures outside the authorized transition retain normal
supervision. This SQL policy does not promise interruption-free replacement of
the whole deployment or transparent blue-green migration of stateful backend
connections.

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

Acceptance: [Host Service Acceptance](#host-service-acceptance) and
[Generation Acceptance](#generation-acceptance).

`cli.md` owns public command placement, root help behavior and the packaged
binary surface. This spec owns workspace information/start/stop/GC, explicit
host information/stop/GC and intrinsically host-user-wide update,
logs/endpoints, bare Dolt information and Dolt endpoint/readiness operations
(`socket`, `port`, `host`, `wait`, `sandbox-endpoint`).
[CLI Dolt Information](cli.md#dolt-information) owns the information default
replacing status and the print-only attach helper.
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

Service implementation is Rust-first: `wrix-service` owns deployment/generation
lifecycle, workspace registration, shared role endpoint metadata and
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
host-loopback port and publishes the exact endpoint through
`wrix service endpoints` and worker projections. Linux does not publish a Dolt
TCP port by default.

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

The controller publishes one shared host-loopback cache-read endpoint with
native authenticated private/common view URLs and exact per-launch grants. Host
publishers use separately authorized routes, not host Nix substitution, under
[Host Publication-only Integration](cache.md#host-publication-only-integration).

Preferred ports belong to deployment roles within cache HTTP `21000–22999` and
fallback Dolt TCP `23000–24999`, not to workspace hashes. Busy-port fallback is
actually bound, persisted and reused while available; metadata alone is not
publication. Exhaustion or inability to bind loopback-only fails. Launchers
inject proven sandbox-visible routes without container DNS. Namespace
registration reserves no additional TCP port.

Unexpected exit of an active proxy, Dolt, Celler or shared sccache daemon fails
its affected generation container rather than hiding an unavailable component.
Intentional namespace retirement, completed generation retirement and authorized
backend/ownership handoff are not unexpected exits. Failure must be reported and
dependent access must fail closed; it is not permission to resurrect revoked
grants or bypass failed readiness. Components still sharing that container can
affect other workspaces; generation separation is not a promise of independent
tenant or component compromise/fault isolation.

## Success Criteria

### Host Control Acceptance

- One host-user controller serializes deployment and namespace ownership through
  authenticated typed local operations, excluding guest control, arbitrary
  execution, paths, mounts and forwarding destinations.
  [test](../crates/wrix-service/tests/host_control.rs::control_operations_are_typed_authenticated_and_host_only)
- Issuer/management state remains outside service mounts/data routes; container
  observations cannot authorize host paths or prove backend ownership release.
  [test](../crates/wrix-service/tests/host_control.rs::private_authority_is_not_delegated_to_service_observations)
- Rama fixed-role routes pin established connections while selection changes new
  connections, without SQL parsing, protocol sniffing or general proxying.
  [test](../crates/wrix-service/tests/host_control.rs::fixed_role_routes_preserve_connections_without_general_proxying)
- Native controller lifecycle adapters serialize start, preserve explicit-stop
  intent, pin active package closures and reject unsupported supervision without
  an unsupervised fallback.
  [test](../crates/wrix-service/tests/host_control.rs::supervision_preserves_stop_intent_and_active_closures)
- Linux/Darwin user supervision preserves caller-independent singleton lifetime;
  explicit host stop prevents resurrection and temporary Nix provision retains
  the active controller closure.
  [system?](test-ci:test-services-host-control-lifecycle)
- Private control compatibility is admitted before effects, and installing a new
  host package cannot replace a running controller.
  [test?](../crates/wrix-service/tests/host_control.rs::client_compatibility_does_not_authorize_replacement)
- Compatible backend updates retain the controller; controller replacement
  requires explicit host stop/update and ordinary start resumes recorded
  versions without transparent-router-replacement claims.
  [test?](../crates/wrix-service/tests/host_control.rs::controller_replacement_requires_explicit_maintenance)
- Restart reconciles transition records and observed owners before admission or
  retirement, without concurrent backend opens, grant revival, replay or
  fabricated execution completion.
  [test?](../crates/wrix-service/tests/host_control.rs::restart_preserves_uncertainty_and_single_backend_ownership)
- Namespace registration uses shared role listeners without dynamic runtime port
  publication, serving mount changes or container replacement.
  [test?](../crates/wrix-service/tests/host_control.rs::namespace_registration_uses_existing_role_listeners)
- Native shared SQL authorization denies peer database/file-remote access and
  unrestricted filesystem/network operations, including after namespace stop.
  [system?](test-ci:test-services-shared-sql-tenancy)
- Real Podman/krun guests reach fixed-role routes with loopback-only host
  publication, known-live wrong-route controls and honest capability reporting.
  [system?](test-ci:test-services-host-control-podman)
- Real Apple guests complete authenticated SQL/cache requests through fixed-role
  routes with unchanged serving mounts and no cross-VM Unix assumptions,
  wildcard publication or routine privileged route repair.
  [system?](test-ci:test-services-host-control-apple)

### Compiler Service Acceptance

- The shared daemon uses one service-owned native LRU cache defaulting to `50G`;
  ordinary worker/repository/spawn settings cannot resize it, while host/private
  local caches remain independent and other disk data is not claimed as capped.
  [test?](../crates/wrix-service/tests/compiler_service.rs::shared_compiler_capacity_is_service_owned_and_defaults_to_fifty_g)

- Service planning/image integration hosts one actual sccache daemon alongside
  the other service families, without an extra compiler-only isolation
  container/VM or HTTP blob-cache backend.
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

- Fresh Celler creation uses host-user ownership without binding the backend to
  the first requesting repository; workspace SQL startup does not acquire the
  shared cache's persistent mounts.
  [test](../crates/wrix-cli/tests/service_lifecycle.rs::public_cli_shared_native_backend_preserves_namespaces_and_denies_peer_sql)
- Host ownership metadata roundtrips without a workspace identity.
  [test](../crates/wrix-service/src/lifecycle/owner/test.rs::host_owner_roundtrips_without_a_workspace_identity)

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

- Concurrent ordinary starts across workspaces select one host-user deployment
  without competing generation activation and independently register namespaces.
  [test?](../crates/wrix-service/tests/generation_handoff.rs::concurrent_starts_preserve_one_deployment_selection)

- Apple sandbox Dolt endpoint publication is guest-reachable and stable across
  authorized routing changes, fails if no safe reachable route exists, and keeps
  physical-host publication loopback-only.
  [test?](../crates/wrix-service/tests/generation_handoff.rs::apple_endpoint_is_stable_guest_reachable_and_host_loopback_only)

- Workspace identity produces stable namespaces, shared role endpoint
  references, workspace control/data roots and locks without a container
  identity per checkout
  [test?](../crates/wrix-service/tests/host_service.rs::workspace_namespaces_are_stable_and_distinct)

- `mkDevShell` starts the service container by default for the project cache,
  `nixCache = false` suppresses cache-only startup, and any service container
  survives the process that evaluated the shell hook
  [system](test-ci:test-services-devshell-start-independent)

- `wrix service start` delegates image installation/digest reuse to the shared
  installer using `image-builder.md` source metadata without broad image
  pruning, MRU-eight eviction or a separate service-specific installer
  [test](../crates/wrix-cli/tests/service_cache.rs::service_start_uses_shared_image_digest_preflight_without_pruning)

- Cache-only service startup is suppressed for temp-directory scratch
  workspaces, so tests and integration runs do not create infrastructure solely
  for scratch cache use
  [test](../crates/wrix-cli/tests/service_cache.rs::temp_cache_only_start_never_creates_a_service_container)

- Independent nested bead clones have independent standalone service identity;
  membership sharing follows [Workspace Admission](workspaces.md).
  [test](../crates/wrix-service/tests/lifecycle.rs::nested_bead_clone_has_an_independent_service_identity)

- Independent nested integration clones have independent standalone service
  identity regardless of `.loom` naming.
  [test](../crates/wrix-service/tests/lifecycle.rs::nested_integration_clone_has_an_independent_service_identity)

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

- Unexpected component exit fails its affected container and dependent access
  closed; intentional retirement/authorized handoff does not terminate unrelated
  service work or revive grants across generations.
  [test?](../crates/wrix-service/tests/generation_handoff.rs::supervision_distinguishes_planned_transitions_from_container_failure)

- Cache HTTP and fallback Dolt TCP use deployment-role ranges; busy fallback is
  bound, persisted and reachable without workspace TCP leases, and inability to
  bind loopback-only fails.
  [test?](../crates/wrix-service/tests/host_service.rs::role_port_leases_are_shared_reachable_and_loopback_only)

- Two independent workspaces share one host-user deployment without replacing
  its serving generations when a new workspace registers; their workspace
  authorization/control roots remain distinct while role listeners are shared.
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

### Dolt Handoff Acceptance

- Candidate preparation leaves live databases unopened and unmigrated, without
  claiming production readiness; normal startup, information, preflight and GC
  cannot authorize a version handoff or override its maintenance ownership.
  [test?](../crates/wrix-service/tests/dolt_handoff.rs::preparation_and_ordinary_operations_do_not_mutate_live_databases)
- New SQL admission closes across every affected namespace, live gateway
  generation and transport/maintenance route, with in-flight connections
  included atomically in drain accounting.
  [test?](../crates/wrix-service/tests/dolt_handoff.rs::sql_admission_gate_covers_all_routes_and_connection_races)
- New connections fail promptly without backend dispatch, queued SQL, replay or
  HTTP error responses on native SQL transports.
  [test?](../crates/wrix-service/tests/dolt_handoff.rs::draining_rejects_new_connections_without_dispatch_or_queueing)
- Existing sessions remain usable until they close; idle or transaction-owning
  sessions prevent drain completion rather than being treated as safe to kill.
  [test?](../crates/wrix-service/tests/dolt_handoff.rs::drain_preserves_existing_sessions_and_waits_for_idle_connections)
- Quiescence requires backend completion and full relevant-session visibility;
  client disconnection, failed inspection or an unrelated observer connection
  cannot falsely prove an empty backend.
  [test?](../crates/wrix-service/tests/dolt_handoff.rs::quiescence_requires_backend_evidence_not_client_socket_counts)
- Drain timeout aborts before old-backend shutdown or update-driven data
  changes, reopens its admissions and reports failure without force-closing
  sessions or stealing locks.
  [test?](../crates/wrix-service/tests/dolt_handoff.rs::drain_timeout_aborts_update_and_resumes_old_admission)
- The new version opens persistent data only after confirmed clean shutdown and
  release by the old owner, never through concurrent read-only/read-write opens
  or lock/mount-role bypass.
  [test?](../crates/wrix-service/tests/dolt_handoff.rs::backend_ownership_transfers_only_after_confirmed_release)
- Potentially incompatible migrations require a consistent recovery checkpoint;
  failed post-stop/migration steps keep admission closed and cannot trigger an
  unverified old-binary rollback.
  [test?](../crates/wrix-service/tests/dolt_handoff.rs::migration_and_startup_failures_preserve_safe_recovery_boundaries)
- Successful admission reopening requires authenticated readiness in affected
  namespaces, preserving data and authorization rather than merely accepting
  TCP, recreating databases or replacing credentials.
  [test?](../crates/wrix-service/tests/dolt_handoff.rs::reopening_requires_authenticated_namespace_readiness)
- Failure reporting distinguishes an aborted drain from a stopped/migrated
  backend needing recovery, without SQL replay or claims that uncertain writes
  have been transparently migrated.
  [test?](../crates/wrix-service/tests/dolt_handoff.rs::handoff_failures_report_phase_without_replaying_sql)
- Real Podman gateway/Dolt handoff preserves completed Beads writes and history,
  drains active sessions, rejects new admissions and leaves worker proxy grants,
  cache access and compilation unaffected; busy-session timeout preserves the
  old backend and a successful retry reopens only after native readiness.
  [system?](test-ci:test-services-dolt-handoff-podman)
- Real Apple gateway/Dolt handoff satisfies the same data, drain, timeout and
  peer-availability contract using its actual TCP and persistent-storage path,
  without assuming simultaneous named-volume attachment or cross-VM Unix
  sockets. [system?](test-ci:test-services-dolt-handoff-apple)

### Generation Acceptance

- Explicit update requires host management authority over this deployment;
  workspace input, cache/proxy traffic credentials and ordinary startup cannot
  authorize generation activation.
  [test?](../crates/wrix-service/tests/generation_handoff.rs::explicit_update_requires_host_management_authority)
- Managed update resolves its candidate from the invoked host package's explicit
  binding, not project launch data, cwd, executable/store-path hashes, version
  strings, recorded selection or ambient image defaults; equivalent bindings
  reuse the same compatible target despite different package paths.
  [test](../crates/wrix-service/tests/generation_handoff.rs::update_target_uses_explicit_package_image_binding)
- Actual Nix-packaged host Wrix supplies its service-image binding independently
  of both projects' generated launch configurations and installation directory;
  production update argv does not adopt either project's image overrides or
  require launcher-only profile configuration.
  [system?](verify:services.managed-update-binding)
- An absent deployment is rejected before candidate creation, namespace
  provisioning or selected-state changes, with ordinary-start guidance.
  [test?](../crates/wrix-service/tests/generation_handoff.rs::update_rejects_absent_deployment_without_creation)
- An existing stopped deployment can be explicitly started/activated at the
  candidate version only through update, with readiness/failure reported rather
  than a successful staged-but-unvalidated selection.
  [test?](../crates/wrix-service/tests/generation_handoff.rs::explicit_update_starts_existing_stopped_deployment)
- The update plan requires gateway, Dolt, Celler and shared sccache readiness at
  the target, including idle/stopped roles and roles unused by the current
  repository; no role is skipped or made optional because it lacks traffic, and
  the update does not enable disabled workspace cache/tool features or broaden
  their permissions.
  [test?](../crates/wrix-service/tests/generation_handoff.rs::update_plan_requires_all_four_roles_without_enabling_workspace_features)
- Concurrent and repeated updates preserve one selection and one owner per
  backend; an already-applied compatible target does not churn generations or
  revoke grants while pending safe retirement can progress.
  [test?](../crates/wrix-service/tests/generation_handoff.rs::concurrent_and_repeated_updates_preserve_selection_and_owners)
- Selection and backend role versions survive stop and ordinary startup resumes
  them; idle/stopped state does not retire the last gateway, select a new image
  or silently migrate a still-Blue backend after partial update. Ordinary/direnv
  activation cannot perform an implicit upgrade.
  [test?](../crates/wrix-service/tests/generation_handoff.rs::selection_survives_stop_without_implicit_upgrade)
- Stable sandbox proxy endpoints select Green for new egress connections while
  retaining old proxy connections on Blue, without agent environment,
  firewall-rule, credential, restart or DNS-cache intervention and without
  broadening internal routes.
  [test?](../crates/wrix-service/tests/generation_handoff.rs::stable_routing_switches_new_connections_without_rewriting_workers)
- Live ownership handoff is required before Green-dependent proxy routing;
  candidate/transfer failure cannot grant unknown access or falsely claim
  readiness or whole-update rollback.
  [test?](../crates/wrix-service/tests/generation_handoff.rs::routing_activation_requires_ready_authorized_ownership)
- An old generation remains while protected egress connections or SQL sessions,
  coordination, backend roles or in-flight work remain; transferred launches
  alone do not pin it. Retirement cannot force-close protected connections or
  kill backend work; proven-idle Celler/sccache transports use backend
  work-drain rules.
  [test?](../crates/wrix-service/tests/generation_handoff.rs::retirement_tracks_generation_work_not_whole_launch_lifetime)
- A formerly serving obsolete generation is removed only with a ready selected
  replacement, preserving persistent backend data/images and reporting
  activation separately from still-draining work and eventual retirement.
  [test?](../crates/wrix-service/tests/generation_handoff.rs::retirement_preserves_data_and_reports_draining_separately)
- Safe retirement continues after the update client exits: once remaining work
  and ownership obligations finish, the obsolete generation is removed without
  another Wrix invocation, GC or whole-worker shutdown.
  [test?](../crates/wrix-service/tests/generation_handoff.rs::retirement_continues_after_update_client_exit_without_followup_command)
- Uncertain eligibility prevents removal; cleanup failure preserves the safety
  guards and reports actual incomplete/failed retirement, not completed cleanup
  or a reason to force-close old work.
  [test?](../crates/wrix-service/tests/generation_handoff.rs::retirement_failure_preserves_guards_and_reports_incomplete_cleanup)
- Dolt, Celler and compiler data remain independently owned persistent areas,
  not concurrently opened writable state in both colors or an assumed
  parallel-attach Apple named volume.
  [test?](../crates/wrix-service/tests/generation_handoff.rs::generation_storage_preserves_per_backend_single_ownership)
- Real Podman generations switch new sandbox proxy connections transparently,
  preserve an established slow transfer/CONNECT connection on Blue, revoke
  transferred ownership across colors and, after the update CLI has exited,
  automatically remove Blue when its remaining work drains, without another Wrix
  invocation. [system?](test-ci:test-services-generation-handoff-podman)
- Real Apple generations satisfy the same transparent routing, revocation,
  automatic post-CLI drain/removal and persistent-state contract through actual
  guest TCP and supported mount behavior, without assuming cross-VM Unix sockets
  or requiring another Wrix invocation.
  [system?](test-ci:test-services-generation-handoff-apple)

### Backend Upgrade Acceptance

- Celler handoff drains admitted transfers/publication and background metadata
  work, rejects new work during the gap, then cleanly releases one owner and
  authenticates the replacement without key/view/retention reset.
  [test?](../crates/wrix-service/tests/backend_handoff.rs::celler_handoff_drains_native_work_and_preserves_authority)
- Sccache handoff waits for native jobs, originating outputs and cache writes,
  transfers its same 50G single-server cache and checks native readiness without
  replay, peer-output collision or job killing.
  [test?](../crates/wrix-service/tests/backend_handoff.rs::compiler_handoff_preserves_jobs_outputs_and_single_cache_ownership)
- Proven Celler workflow/transfer/background-write completion permits clean
  shutdown to close idle HTTP transports; an admitted publication between
  requests or unfinished metadata work is not idle or safe to interrupt.
  [test?](../crates/wrix-service/tests/backend_handoff.rs::celler_idle_transport_shutdown_preserves_publication_workflows)
- Proven compiler job, output-delivery and cache-write completion permits clean
  shutdown to close idle compiler transports without waiting for whole worker
  lifetimes; pending output or writes cannot be discarded as idle.
  [test?](../crates/wrix-service/tests/backend_handoff.rs::compiler_idle_connections_do_not_pin_work_drained_handoff)
- Busy timeout or uncertain native completion aborts the affected backend before
  update-driven stop/data changes and reopens old admission, without killing
  work or closing protected connections; socket-idle guesses are insufficient,
  and GC budgets do not silently set upgrade drain budgets.
  [test?](../crates/wrix-service/tests/backend_handoff.rs::busy_timeout_preserves_old_backend_without_forced_drain)
- Post-release/migration failures retain recovery state and unready admission;
  reports distinguish completed/draining/failed roles and nonzero incomplete
  required work without unsafe downgrade or forced old-container removal.
  [test?](../crates/wrix-service/tests/backend_handoff.rs::partial_updates_report_role_state_without_unsafe_rollback)
- Real native Celler and compiler upgrades preserve admitted transfers, signed
  scoped reads, jobs/output delivery and compatible cache reuse while a
  long-lived worker continues using stable endpoints. Proven-idle Celler and
  compiler transports can close only after work drains, and native clients
  reconnect at those endpoints; busy/uncertain and startup failures exercise the
  production recovery/reporting path without killing active work.
  [system?](test-ci:test-services-backend-handoff)

## Requirements

Acceptance: [Host Service Acceptance](#host-service-acceptance),
[GC Scope Acceptance](#gc-scope-acceptance),
[Compiler Service Acceptance](#compiler-service-acceptance),
[Compatibility Acceptance](#compatibility-acceptance) and
[Dolt Handoff Acceptance](#dolt-handoff-acceptance),
[Generation Acceptance](#generation-acceptance) and
[Backend Upgrade Acceptance](#backend-upgrade-acceptance).

### Functional

1. **Host and workspace identity** — one deployment belongs to the host user;
   namespaces follow [Architecture](#architecture) without data/access/lock
   collisions.
2. **Service operations** — lifecycle and Dolt operations follow
   [Service Operations](#service-operations); explicit collection scope follows
   [Garbage Collection Scope](#garbage-collection-scope),
   [GC Availability](#gc-availability) and [GC Contention](#gc-contention).
   Cache semantics belong to `cache.md`.
3. **Lifecycle management** — shared creation, namespace registration, read-only
   information and scoped stop follow [Lifecycle](#lifecycle), independently of
   caller lifetime.
4. **Compatibility and replacement** — reuse/failure follow
   [Compatibility and Replacement](#compatibility-and-replacement); explicit
   generation activation/routing/retirement follow
   [Generation Handoff](#generation-handoff).
5. **Service image source** — creation uses the shared installer described in
   [Service Image Source](#service-image-source), not a service-specific path.
6. **Endpoint publication** — selected-workspace SQL and cache access follow
   [Dolt Service](#dolt-service) and [Cache Endpoints](#cache-endpoints).
7. **Shared compiler** — admitted job context, the single native cache budget,
   lifecycle and trust boundary follow
   [Shared Compiler Service](#shared-compiler-service).
8. **Dolt version handoff** — SQL admission, connection draining, single-owner
   data transfer and truthful recovery follow
   [Dolt Backend Handoff](#dolt-backend-handoff).
9. **Cache/compiler upgrade** — native work draining, single-owner transfer and
   partial outcomes follow
   [Cache and Compiler Backend Handoff](#cache-and-compiler-backend-handoff).

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
  excluded; delegated compiler jobs and admitted [Nix Realization](store.md) are
  execution capabilities, not worker-runtime migration.
- Independent container-level compromise/fault isolation between cohosted
  service components or workspace tenants.
- Host-native Dolt and host OS account/module provisioning for a native Dolt
  daemon. Moving only Dolt does not eliminate the proxy's service container and
  would add a separate privileged setup and lifecycle.
- Automatic replacement/upgrade of a running host service from an ordinary
  workspace startup or direnv reload. Generation selection changes only through
  explicit host-authorized activation; ordinary restart resumes the selection.
- Transparent SQL transaction/session migration, SQL replay, queued SQL
  admission during Dolt handoff, live read-only/read-write data-role flipping,
  or closing protected egress/SQL connections or killing admitted backend work
  to guarantee an upgrade completes.
- Automatic idle shutdown, background execution reconciliation or recovery of
  agent work; host-wide shutdown is explicit.
- Cache contents, transport/signing/publication/retention and cache exclusions
  beyond hosting/namespace/endpoint duties; those belong to `cache.md`.
