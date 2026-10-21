# Project Nix Cache

Celler-backed binary-cache transport, scoped visibility, signing, publication
and retention for host publication and sandbox substitution.

## Problem Statement

Cold sandboxes need usable project outputs without rebuilding them or acquiring
host store/daemon/publishing authority. The cache publishes explicitly scoped
host outputs, supports authenticated read-only substitution, and bounds retained
state. Physical archive deduplication does not grant access to another
workspace.

## Architecture

Celler is the binary-cache backend, packaged into the shared host-user service
container alongside Dolt and the egress proxy. It owns cache metadata, archive
storage and per-cache Nix signing. `wrix-cache` owns Wrix policy, the canonical
host-build publication pipeline, publisher/hook integration and scoped read
admission; `wrix-cache-hook` and `wrix-cache-publish` remain immutable Nix entry
points. There is no parallel Wrix flat-file static server or transport fallback.

Celler is selected in part to reuse its pure-Rust Nix store client. The host
publisher uses that client for store metadata queries and NAR streaming; Nix CLI
root discovery and flake evaluation remain allowed. This host-side integration
does not expose the host store or daemon to sandbox clients. Cache integration
uses the existing sandbox runtimes; it does not require a shared worker VM,
containerd or nix-snapshotter migration.

`services.md` owns host-user hosting, workspace identity/registration,
lifecycle, compatibility, mount isolation and endpoint publication. `cli.md`
owns command grammar; operations remain under `wrix service cache ...`.
`profiles.md` supplies the devshell adapter, and `sandbox.md` integrates
launch-time cache transport. `security.md` owns the cross-cutting trust model.

Acceptance: [Celler Backend Acceptance](#celler-backend-acceptance),
[Visibility Acceptance](#visibility-acceptance),
[Population Acceptance](#population-acceptance),
[Build Pipeline Acceptance](#build-pipeline-acceptance),
[Freshness Acceptance](#freshness-acceptance),
[Cache Acceptance](#cache-acceptance) and
[Signing Acceptance](#signing-acceptance).

### Cache Visibility

Acceptance: [Visibility Acceptance](#visibility-acceptance).

One host-user Celler instance provides these logical views to ordinary cache API
clients. Shared compiler execution is a separate, broader service trust
capability under [Security](security.md#shared-service-trust-boundary), not
confidentiality isolation supplied by these read grants:

| View                       | Read audience                               | Content boundary                                                       |
| -------------------------- | ------------------------------------------- | ---------------------------------------------------------------------- |
| Common public dependencies | Wrix workspaces using this host-user cache  | Explicitly approved public dependencies, not arbitrary project outputs |
| Repository-private         | The explicitly selected workspace namespace | That repository's private cache contents                               |

Private-view identity follows [Workspace Admission](workspaces.md),
independently of caller checkout location or external-manager directory naming.
A workspace's read set is the authenticated common view and its own private
view, with exact endpoints, public keys and pull credentials. Existing grants
follow [Native Pull Credentials](#native-pull-credentials), not immediate
revocation when host authorization changes. Public reuse is across Wrix
workspaces, not unauthenticated Internet/LAN serving.

Host management controls view provisioning and publication audiences. Agent
requests, repository edits, supplied cache names and opaque configuration cannot
broaden the read set or classify private output as public. Pull access never
grants publishing, signing, rotation or management authority.

Physical NAR/chunk sharing creates no private-view trust union. Knowing a
private path/hash or reading the common view cannot expose a private-only
object. Workspace stop and private-key rotation preserve the common view and
unrelated private views. Collection follows native age eligibility, preserving
bytes still referenced by an unexpired view under
[Cache Eviction](#cache-commands-locking-and-retention).

Common population follows
[Verified Public Population](#verified-public-population); normal private
publication/upstream filtering remains in place. No sharing-group view,
group-target publication or worker upload is part of this contract.

### Native Pull Credentials

Acceptance: [Pull Credential Acceptance](#pull-credential-acceptance).

Use Celler native signed bearer tokens for sandbox pull access with one-day
(24-hour) nominal lifetime and exact named-view pull grants. All views,
including common, remain authenticated. The host controller retains the private
issuer key; Celler receives JWT verification authority separately from Nix
signing keys. There are no host pull tokens or host-activation pull-credential
refresh.

Issuance/renewal rechecks current host-controlled authorization, not repository
requests or stale grants. Withdrawal prevents subsequent issuance for that view,
but already-valid tokens remain usable until native expiry, including reads of
new contents. Bearers are reusable by any client that can reach the endpoint;
repository labels and Basic Auth usernames do not bind them. No per-request host
policy lookup, denylist, active-download cancellation or recall of imported data
is promised. JWT verification-key rotation differs from Nix signing-key
rotation.

Waiting launchers schedule renewal under
[Launcher-Owned Renewal](#launcher-owned-renewal). The controller issues on
authorized requests; persistent controller lifetime does not create detached
background renewal.

### Host Publication-only Integration

Acceptance:
[Host Consumption Exclusion Acceptance](#host-consumption-exclusion-acceptance).

Cache flow is one-way: admitted trusted host outputs are published to Celler;
sandbox-side Nix consumes them into its admitted store. Wrix-managed host Nix
contexts do not add this shared cache as a substituter, trust its signing keys,
issue host pull tokens or pull through an alias/alternate URL. Host builds
retain their independently trusted upstreams and ordinary local-store reuse.

Host publishers use scoped upload/management authority and verified local
metadata/NAR bytes. Backend responses are untrusted protocol input, not
authority to import artifacts, execute code, classify public data or approve
build/check evidence. Read-only inspection and opt-out do not issue credentials
or delete private state. Host publication/hook setup is separate from sandbox
pull authentication.

Generated configuration excludes all service-held cache signing keys from host
trust, not just worker-upload keys. Known effective inherited shared-cache
substitution/trust blocks enabled host integration with explicit removal
guidance; do not mask it with a fresh generated configuration. Operator-managed
Nix settings are not silently rewritten. Removing trust does not recall or
attest previously imported outputs; automatic store deletion or machine-wide Nix
rewrites are not authorized.

### Launcher-Owned Renewal

Acceptance: [Launcher Renewal Acceptance](#launcher-renewal-acceptance).

For cache-enabled run/spawn, the original host launcher automatically refreshes
pull credentials before expiry while it waits for its runtime child, including
sessions lasting multiple token lifetimes. Renewal rechecks current host policy
for that launch's canonical workspace; it does not copy stale token grants. No
separate host daemon or agent-held issuer authority is needed.

Deliver credentials through a private per-launch host directory mounted
read-only into the sandbox. Native Nix uses a stable `netrc-file` path there;
the host atomically replaces the file inside the directory so subsequent reads
see complete replacement credentials without restarting the agent/container. Do
not freeze tokens into environment values or a launch-only file snapshot, mount
only a file inode that cannot observe replacement, or expose another launch's
credentials or host issuer keys.

Renewal retries transient failures with redacted diagnostics outside agent
protocol stdout. Failure does not kill/restart the agent or service, widen
permissions or extend an old token's expiry. Scheduling rechecks absolute token
expiry after suspension/clock changes; recovery can still race a native Nix
request, so uninterrupted cache authentication after a long suspension/outage is
not guaranteed. Exact proactive scheduling/retry intervals are implementation
judgment, not a new user setting.

Normal launch completion stops renewal and cleans up per-launch credentials.
Renewal does not survive launcher exit via a detached manager, and owner exit
does not revoke an already-issued cache token before native expiry. Caller-owned
backgrounding remains allowed; managed detach/reattach and connection-owned
proxy grant semantics are unchanged.

### State Layout

Acceptance: [Celler Backend Acceptance](#celler-backend-acceptance) and
[Cache Acceptance](#cache-acceptance).

Host-private workspace control state uses the platform-native state root defined
by [Services State Layout](services.md#state-layout). Control metadata authority
and explicit import follow
[Workspace Control Storage](workspaces.md#native-control-storage). Host cache
status, publication manifests, deferred candidates and closure lists live only
in the native-root `state.db`, alongside service metadata. Ordinary host
operations do not materialize JSON or `pending/` and `gcroots/` metadata
exports. The remaining per-workspace native resources include:

```text
<state-root>/
├── cache.lock              # namespace publication/rotation lock
├── service.lock            # service lifecycle lock
├── keys/cache.secret       # private publication credential
├── keys/cache.pub          # repository-private view's public key
├── hook-wrapper/           # generated hook package build inputs
└── host-nix-roots/         # actual Nix retention roots for hook/runtime packages
```

Worker-side filesystem projections remain non-authoritative. Legacy host JSON
and closure-list files require explicit supported import; neither import nor
ordinary operations silently rewrite/delete them. Their explicit retirement
follows the workspace storage contract. Closure lists are metadata, unlike the
actual Nix retention roots retained above. Immutable hook configuration
snapshots are runtime package inputs, not mutable host metadata exports.

Host publishing credentials, credential-issuer/management authority and
authoritative manifests stay outside service-visible mounts. Credentials are
runtime state, not immutable hook snapshots, Nix outputs or image layers.

Celler's metadata, per-cache signing state and shared archive storage use
narrowly mounted, host-backed managed service data outside worktrees.
Metadata/signing state is durable; bulky archives use platform-native cache
storage. Both survive namespace stop and container replacement. A new workspace
registers a cache view without replacing the container or adding arbitrary host
mounts. Celler owns its internal database/archive format, not a public flat
`*.narinfo` file layout.

Physical NAR/chunk deduplication is independent of cache membership, keys,
publication and retention. Removing one namespace's membership cannot remove an
archive still required by another namespace. Neither direct sandbox mounts nor
ordinary cache API credentials expose Celler's backing database, signing keys or
all-cache archive tree. These delivery and API limits do not isolate that state
from delegated service-side compiler code under
[Security](security.md#shared-service-trust-boundary).

### Cache Transport

Acceptance: [Celler Backend Acceptance](#celler-backend-acceptance),
[Freshness Acceptance](#freshness-acceptance),
[Cache Acceptance](#cache-acceptance) and
[Signing Acceptance](#signing-acceptance).

Workspace endpoint authorization follows
[Services Cache Endpoints](services.md#cache-endpoints). Cache trust, publishing
roots and retention remain scoped to their authorized views under
[Cache Visibility](#cache-visibility); shared serving creates no implicit union
of private views or trust.

- **Host reads** use independently trusted upstreams/local store, never this
  shared service cache or its signing keys.
- **Host writes** upload only explicitly selected eligible paths to Celler using
  host-controlled namespace publishing credentials and record actual publication
  outcomes. Celler signs responses under that cache's key.
- **Sandbox reads** use launcher-resolved endpoint(s), the public keys of the
  authorized views and runtime-only scoped pull credentials. Native Nix retains
  `builders-use-substitutes = true`; no peer private key/view is implicitly
  trusted.
- **Sandbox writes** through the cache interface are excluded: ordinary cache
  delivery supplies no publishing/management credentials, signing keys or
  write-capable route. This is not protection against delegated compiler code
  executing inside the shared service.

The agent-facing read endpoint permits only GET/HEAD for Nix binary-cache
resources in its selected authorized view. It rejects mutation/admin routes,
directory listing, unrelated files, traversal and unauthorized cross-view
routing. Credential checks apply to metadata and archive reads, not just cache
inventory. The host publication/management interface requires its separate
scoped authority; it is not an allowed sandbox endpoint or proxy destination.
Source address, a guessed cache name or knowledge of a private NAR hash cannot
grant cache membership. Deduplication never bypasses possession/authorization
checks.

Container DNS is not required: Wrix publishes loopback-only host endpoints and
injects resolved sandbox-visible endpoints. Host publishers and real sandbox Nix
must successfully authenticate through their distinct production interfaces.
Operational readiness authenticates the selected namespace; a mock upload or
reachable TCP port alone is insufficient.

The production read-admission path isolates clients from each other and the
accept loop. It admits at most 32 concurrent requests, bounds headers to 8 KiB
and 32 fields within five seconds, and bounds writes to 30 seconds of inactivity
and five minutes total. GET streams with bounded memory; HEAD does not read
archive bodies. These bounds apply around Celler, not merely to an unused
helper.

HTTP is the cache client transport. Unix-socket cache substituters, host Nix
daemon sockets, shared mutable `/nix/store` volumes and host-store-serving
caches are excluded. Clients still import archives into the consuming store;
Celler is not a shared installed guest store or an automatic host `/nix/store`
view. The separate [Installed Store Plane](store.md) does not change Celler
transport, signing or audiences.

### Client Publication Freshness

Acceptance: [Freshness Acceptance](#freshness-acceptance).

With cache integration enabled, Wrix-generated sandbox Nix client contexts use
`narinfo-cache-negative-ttl = 0`. This native Nix setting applies to every
substituter in that configured context, not only local Celler. Wrix does not
change machine-wide Nix configuration, directly edit client metadata-cache
databases or disable positive metadata caching to obtain freshness. Cache
opt-out does not add or change this setting solely for Wrix cache use.

After successful usable signed publication, the next ordinary Nix command in an
already-running, correctly configured sandbox context can discover and
substitute a previously missing path through its reachable authorized cache. The
caller does not restart the agent, flush metadata, change the endpoint or supply
a manual refresh flag. Native authentication/signature requirements and normal
error handling still apply; publication is not permission to bypass them.

Existing valid store paths remain reusable and positive metadata caching remains
native Nix behavior. This contract does not restart a build already underway or
invalidate previously positive metadata; it addresses cached misses. Nor does it
retrofit configuration into containers launched with an older configuration.

Repeated upstream misses may issue additional requests. Reproducible
measurements compare repeated misses against normal negative caching and
distinguish them from positive-cache/local-store hits, recording request counts
and elapsed time. No claim of local-cache-only invalidation, zero upstream
overhead or performance speedup follows from setting the TTL to zero.

### Cache Authority

Acceptance: [Celler Backend Acceptance](#celler-backend-acceptance) and
[Signing Acceptance](#signing-acceptance).

Celler receives Nix cache signing authority inside the shared service boundary.
Its signing state is not protected from a sufficiently privileged compromise of
a cohosted Dolt/proxy/cache component. Host publishing/management credential
files, token-issuer private keys and authoritative manifests are not mounted
into the service. Authorized requests necessarily deliver scoped authorization
to Celler; a service compromise can observe that runtime authorization.
Unmounted credential files do not establish protection for credentials used by
the server. General host filesystem access and the host Nix daemon are not
delegated.

Host management provisions the views and narrowly scoped publishing/pull
credentials defined by [Cache Visibility](#cache-visibility). Ordinary cache API
credentials, repository input and read endpoints cannot mint broader grants,
select unauthorized views, register a public cache or gain publishing authority.
The accepted service-side compiler capability is outside those API guarantees.
Native Nix signature requirements remain enabled. Sandbox cache consumption does
not require Nix `trusted-users` authority over the host or a shared daemon;
privileged host setup still follows
[Host Nix Integration](#host-nix-integration-and-publish-hook). Signing
authenticates the cache authority, not build correctness.

### Canonical Host Build Pipeline

Acceptance: [Build Pipeline Acceptance](#build-pipeline-acceptance).

All Wrix-managed host Nix build dispatch uses one shared pipeline: selected
verifier/CI realization, host pre-push Nix builds, managed image preparation and
explicit cache warming. Host activation reconciles its already-realized selected
environment under
[Host Entry Points](#host-activation-and-pre-push-publication). Nix remains the
build executor. With cache integration enabled, the pipeline derives publication
candidates from the actual selected build graph; projects do not maintain a
second list of CI runners or reusable build-artifact roots. Existing cache
disablement and applicable explicit publication exclusions remain authoritative.

Selection precedes build-graph discovery. The pipeline inspects only admitted
selected graphs, including their realized build-only dependency outputs and
runtime references. It does not realize unselected browser/image closures,
evaluate all configured roots merely to publish a scoped build, scan the host
store, or build missing outputs solely for cache population. Dependencies that
become realized during the requested build are captured without a second root
declaration. Changes to the selected graph update registration automatically.

Successful completion reconciles realized candidates even when Nix substituted
every output and no post-build hook fired. Hook events alone are not the
completion inventory. Publication retains the verified common/private routing,
upstream filtering, scoped upload and caller error rules below; graph membership
is not proof of publicness or build correctness.

This is host-side coordination, not interception of arbitrary host commands or
central execution of agent builds. Guest-local Nix/Cargo execution remains
unchanged, and sandboxes receive no host publication or management authority. No
new public build-command spelling is required by this contract.

### Host Activation and Pre-push Publication

Acceptance:
[Host Entry Publication Acceptance](#host-entry-publication-acceptance).

With cache integration enabled, host devshell activation automatically admits
and publishes the already-realized selected environment and its dependency
closure through the canonical pipeline. This covers first entry and actual
unchanged/cached direnv activation, including outputs realized before the
post-build hook was installed. It is reconciliation after ordinary environment
preparation, not eager warm: no extra packages/checks, missing output builds,
all-root enumeration or unrelated browser/image realization. Publication follows
actual platform store paths; a Darwin devshell does not seed Linux artifacts
merely by sharing a profile name or trigger cross-platform builds to do so.

Host pre-push Nix realization, including configured `nix flake check` and
selected verifier/CI builds that actually execute, uses the same pipeline.
Capture eligible realized outputs and build-only dependencies, including
substitution-only completion, without a manual cache-root refresh. This works
through installed host hooks without requiring an active devshell. Hook
admission, markers/stamps, Git bypass flags, target selection and
full-versus-scoped verification remain owned by
[Pre-push Hooks](pre-commit.md#host-build-publication) and
[Host Build Dispatch](cli.md#canonical-host-build-dispatch). Skipped checks are
not rebuilt or newly harvested solely to populate cache.

Both entry points retain cache opt-out/exclusions, verified common/private
routing, host-private publishing authority and existing automatic-publication
pending/error rules. No explicit publish/warm command is required. Cache upload
success is not verification evidence, and publication failure cannot hide a
failed check or authorize a pass/approval stamp. Arbitrary Cargo target
directories are not Nix outputs. Guest-local builds remain pull-only and are not
promoted by these host entry points.

### Publication Scope and In-flight Protection

Acceptance: [Build Pipeline Acceptance](#build-pipeline-acceptance).

Selected graph registrations authorize publication; they are not permanent
artifact-retention claims. Canonical graph updates and supplementary-root
refresh preserve other admitted in-flight/configured scopes and pending work
without requiring a cache-pin registry or deleted-target reconciliation for
eviction.

Failed builds/publication report actual pending/error state under the existing
automatic-versus-explicit caller rules. They do not trigger deletion of previous
cache entries. Independent native age-based eviction still applies; publication
is not an atomic all-artifact transaction or an indefinite availability promise.
Publish, warm and graph refresh do not dispatch Celler GC or prune by root
reachability.

Actual Nix GC roots protect in-flight host outputs while needed for publication.
Installed hook/runtime roots remain protected under their owning contracts.
Cache eviction neither removes those roots nor authorizes host-store GC. These
store-liveness protections are distinct from the evictable Celler copies under
[Cache Eviction](#cache-commands-locking-and-retention).

### Project scope and root sets

Acceptance: [Build Pipeline Acceptance](#build-pipeline-acceptance),
[Cache Acceptance](#cache-acceptance) and
[Population Acceptance](#population-acceptance).

Project publication admits the selected graphs registered by the
[Canonical Host Build Pipeline](#canonical-host-build-pipeline) and roots
selected for standalone publish/warm or supplementary hook use. Verified public
candidates route to common under
[Verified Public Population](#verified-public-population); other paths retain
repository-private filtering. No additional group audience is inferred from
graph membership. The standalone commands distinguish **publishable roots** from
**warm roots**:

- Default publishable roots: current-system `packages`, current-system `checks`,
  and the selected `devShell`. If a user runs `nix flake check`, check outputs
  may enter the project cache.
- Default warm roots: current-system `packages` and the selected `devShell`.
  Checks are built by proactive warm only when explicitly requested (for example
  `wrix service cache warm --checks` or matching config).

`nixCache` attrset form lets consumers add/remove standalone or supplementary
roots with symmetric `includeRoots` / `excludeRoots`; excludes win for direct
root selection. Root entries are flake installables or explicit flake attr
paths, not arbitrary `/nix/store` paths. A matching explicit publication
exclusion also blocks direct canonical/activation root admission; these lists
and category defaults are not a required parallel inventory of canonical Wrix
builds.

Root exclusion is not a transitive store-path denylist or a confidentiality
filter. For example, excluding `packages.<system>.sdk` prevents its direct root
admission, but an admitted `checks.<system>.consumer` may still publish that
SDK's realized output as a dependency. Candidates come only from admitted
graphs/closures and retain the ordinary public/private routing and filters.
Discovering those candidates does not evaluate excluded or unrelated roots.

Publishing identifies already-realized host paths from the admitted selected
graph or standalone root closure. It routes verified public candidates to common
rather than discarding them merely because an upstream has them. For remaining
private candidates, paths already available from configured upstream
substituters are still excluded. Availability queries are batched (at most 256
paths per subprocess); JSON null misses remain private candidates, and
successful entries survive partial query failure. Uploads use only the selected
paths without recursive backend expansion.

### Verified Public Population

Acceptance: [Population Acceptance](#population-acceptance).

The trusted host publisher populates the common view from already-realized paths
admitted by [Project Scope](#project-scope-and-root-sets). Canonical graph
completion, scoped hook publication and explicit publish/warm use the same
routing. This classification adds no automatic warm trigger, root discovery
outside the selected/explicit scope or unrelated host-store harvesting.

Public eligibility requires verified identity against an independently
host-approved public upstream. Initially the approved source is
`https://cache.nixos.org`, with its approved Nix signing key. Ordinary
configured substituters are not automatically approved public sources.
Repository inputs, agent-supplied flags, package names, nixpkgs provenance and
build success cannot approve a source or classify a path as public.

The publisher verifies upstream-signed metadata for the exact store path and NAR
hash against the local candidate's identity. Uploaded bytes must match that
verified NAR identity. Missing, malformed, unverifiable or mismatched evidence
never authorizes common publication; it cannot turn an unknown into a public
hit. Common entry metadata must not disclose host-local/private deriver, build
or source metadata: use verified public metadata or omit optional private
fields, not a wholesale copy of locally enriched metadata. Remaining paths
retain the existing private eligibility/upstream filtering and caller
error-handling rules. A hit in a private/unapproved upstream is not public-data
evidence.

| Eligible candidate                                                      | Baseline common/private audience                                      |
| ----------------------------------------------------------------------- | --------------------------------------------------------------------- |
| Realized locally and verified against an approved public source         | Common, even when ordinary upstream subtraction would have skipped it |
| Other realized path                                                     | Repository-private only if existing private upstream filters admit it |
| Unrealized or outside the admitted selected graph/explicit root closure | Not published by this pipeline                                        |

These rules govern common/private routing; read access cannot redirect
publication. Concurrent publishers may reuse the same verified public object
without broadening another workspace's permissions or damaging existing views.

Common population does not build missing outputs, scan the entire host store,
download missing store outputs solely to seed the common view, or accept worker
uploads. Host metadata verification is not a runtime read-through fetcher: a
sandbox cache miss does not cause Celler or its read endpoint to fetch upstream
content. [Cache Eviction](#cache-commands-locking-and-retention) and
[Common Retention](#common-retention) govern expiry.

### Cache policy

Acceptance: [Cache Acceptance](#cache-acceptance).

These are workspace/project settings. Private/common retention overrides are
separate host-managed policy under [Retention Control](#retention-control), not
repository settings or ordinary workspace-environment overrides.

The invoked repository's optional root-level `[cache]` section in `wrix.toml`
supplies cache overrides, including outside a devshell. Missing files and keys
leave built-in defaults intact. Settings resolve field-by-field from built-ins,
then repository policy, then explicitly set environment variables; documented
invocation overrides such as `warm --checks` take precedence. An explicit
built-in value or empty root list still overrides repository policy.
`[cache].enable = false` or `nixCache = false` disables Nix-cache integration;
`run --no-cache` instead selects private tool caches and does not disable
Celler. The policy file is read from the nearest invoked repository,
independently of Loom's shared service identity. Configuration reads do not
provision services or mutate repository files.

Supported keys:

- `enable` and `require_trusted_nix` default to true; environment overrides are
  `WRIX_NIX_CACHE_ENABLE` and `WRIX_NIX_CACHE_REQUIRE_TRUSTED`. Disabling cache
  integration does not disable Dolt or prohibit explicit cache maintenance.
- `publish` and `warm` each accept `packages`, `checks`, `dev_shell`,
  `include_roots`, and `exclude_roots`. Publish enables all three categories;
  warm enables packages and devShell but not checks. Both root lists default to
  empty. Overrides use the existing `WRIX_CACHE_PUBLISH_*` and
  `WRIX_CACHE_WARM_*` variables; root-list variables are newline-separated.
- `warn_size` defaults to `"50G"` and `pending_ttl` to `"7d"`. Overrides are
  `WRIX_CACHE_SOFT_LIMIT_BYTES` and `WRIX_CACHE_PENDING_RETENTION_SECS`.
- Repository `prune_interval` and Nix-adapter `pruneInterval` are not supported.
  Workspace startup/environment cannot schedule host-wide Celler collection;
  `WRIX_CACHE_PRUNE_INTERVAL_SECS` is not a cache-policy override.
- `lock_timeout` defaults to `"30s"`; `explicit_lock_timeout` follows it unless
  independently overridden. Their environment overrides are
  `WRIX_CACHE_LOCK_TIMEOUT_MS` and `WRIX_CACHE_EXPLICIT_LOCK_TIMEOUT_MS`.
- Sizes accept nonnegative byte counts or strings with binary `K`, `M`, `G`, or
  `T` suffixes. Numeric durations are nonnegative second counts; human-readable
  duration strings use `humantime`, not a custom suffix grammar. Values must fit
  their destination range/precision without silent overflow or truncation.
- Unknown keys, malformed values, and overflow fail rather than silently falling
  back.

`mkDevShell` retains its `nixCache` argument as an explicit-override adapter.
Default or empty configuration exports no cache-policy variables; an attrset
exports only supplied keys, including explicit default values and empty lists.
`nixCache = false` emits an explicit disable override. The Nix adapter does not
fill in an independent copy of runtime defaults.

`wrix service cache config` prints the complete resolved settings as JSON
without creating cache state.

### Host Nix integration and publish hook

Acceptance: [Cache Acceptance](#cache-acceptance) and
[Host Consumption Exclusion Acceptance](#host-consumption-exclusion-acceptance).

Enabled devshells and canonical host builds publish eligible selected outputs
without shared-cache substitution, shared signing-key trust, host pull tokens or
a host-only negative-TTL override. Independently trusted upstream settings
remain intact.

The immutable post-build hook still requires supported trusted host-Nix setup.
Ignored required publication settings fail loudly with remediation; removing
host substitution neither makes privileged hooks unprivileged nor gives guests
Nix trusted-user authority. Cache opt-out remains explicit.

The Nix-store wrapper contains only validated non-secret workspace identity,
owner uid/gid, immutable publisher path, manifest location and resolved policy.
Root invocation performs static validation and portable privilege drop before
the user-owned publisher resolves runtime publication credentials. Never
evaluate roots as root, source repository code, bake secrets into outputs or
consult workspace files for authoritative scope. Re-entry installs a matching
immutable policy snapshot.

Retain actual Nix GC roots for the configured wrapper/helper/interpreter.
Host-private publication records in `state.db` admit selected canonical graphs
and configured supplementary roots; hook events cannot enroll unrelated builds.
The hook's manifest-path argument identifies that record, not a required host
file. The empty-manifest reminder reads the CLI's publication-root count rather
than probing an obsolete filesystem export. Canonical completion reconciles
substitution-only builds. Standalone publish/warm refresh only their component,
preserving in-flight registrations. First and real cached activation reconcile
already-realized selected outputs without a manual manifest refresh or broad
root scan. A non-Wrix post-build hook is rejected rather than silently
replaced/chained.

### Cache commands, locking, and retention

Acceptance: [Cache Acceptance](#cache-acceptance) and
[Retention Acceptance](#retention-acceptance).

Bare `wrix service cache` reports the selected workspace's effective cache
policy and recorded publication/pending status from existing local metadata. It
labels unavailable or stale details rather than claiming live readiness or that
previously published artifacts are still retained. It does not create managed
state, register a namespace, start/repair services, issue credentials, query
SQL, probe the cache backend, publish, collect or expire pending records. Output
excludes credential contents and other secret values. Existing explicit cache
commands retain their contracts; this default adds no host-scoped cache
information workflow.

`wrix service cache publish` is realized-only: it evaluates configured publish
roots, refreshes that admission component of the publication records, publishes
already-realized eligible paths, drains matching pending candidates and reports
unrealized roots without building them. Canonical graph registrations remain
independent of this supplementary-root refresh.

`wrix service cache warm` is explicit/manual only, not automatic on devshell
entry or launch. It builds configured warm roots through the canonical pipeline
and publishes eligible outputs; `--checks` or configuration includes checks.
Neither command prunes cache contents or runs Celler GC. Explicit commands fail
nonzero on real errors.

Publication and key rotation take `<state-root>/cache.lock`. The automatic
publisher reports waiting; on timeout it saves `DRV_PATH`/`OUT_PATHS` in durable
database pending records (worker observers use `pending/` JSON), warns and exits
0 rather than failing the completed build solely for cache contention. Automatic
publication errors are reported; explicit cache commands remain strict. Pending
candidates default to seven-day retention and are reported by cache status. This
namespace lock policy is separate from the host-GC acquisition budget in
[Services GC Contention](services.md#gc-contention).

Celler copies are an evictable optimization, not an artifact archive. Both
private and common views use Celler's native age-based eligibility from creation
and last access. Private views require a finite host-managed native policy;
repository input cannot restore indefinite current-target pins. Merely remaining
a current target, configured root or registered graph does not prevent expiry.
Returning projects may need upstream substitution or rebuilding. Host
configuration must supply an eviction policy rather than silently inheriting
disabled eviction. Independent private/common overrides follow
[Retention Control](#retention-control), with the common default under
[Common Retention](#common-retention).

Native Celler collection removes expired view entries and reclaims physical data
only when no retained view or native in-flight reference still requires it. It
preserves valid signing/issuer authority and does not delete source
repositories, issue history or installed client store paths. No Wrix cache-root
marker registry, transactional retention-claim replacement or root-reachability
pruning governs it. Actual Nix roots protecting tools, running runtime data and
publication inputs remain separate and are not removed by cache expiry.

Manual Celler collection is host-user-wide through `wrix service gc --host` or
`wrix service gc all`, with scope, availability, waiting and truthful partial
outcomes owned by [Services GC](services.md#garbage-collection-scope). One
native cycle need not exhaust all eligible garbage: batching/concurrent activity
can leave work, and output must not claim an exhaustive drain.
Collection/storage errors or incomplete execution are not successful required
work. Native periodic collection uses host-managed backend scheduling, not
per-workspace startup or publication. `wrix service cache prune` is not a
compatibility alias or a workspace collection bypass.

`wrix service cache rotate-key` takes the namespace lock, invalidates/wipes only
that repository-private Celler view, creates a new keypair and updates metadata.
Repopulation is by publishing/warm, without old+new trust overlap or in-place
new-key re-signing. Same-valid-key unsigned-entry repair follows
[Signing and Repair](#signing-and-repair), not rotation.

`wrix service cache status` reports namespace size, pending count/age,
publication outcomes/errors and endpoints without mutation. The default soft
warning is 50 GiB, not a hard cap or a trigger for forced eviction. Host
collection results belong to the host maintenance report, not a fictitious
workspace-prune result.

### Retention Control

Acceptance: [Retention Acceptance](#retention-acceptance).

Retention is host-managed policy outside mutable repository, devshell and agent
input. The CLI selects one view:

| Command scope                                          | Selected policy                                                                                                                                       |
| ------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| `wrix service cache retention [set <duration>\|reset]` | Current canonical workspace's private view. Loom clones share their outer repository identity; non-repository workspaces use canonical-path identity. |
| The same information/set/reset forms with `--host`     | This host user's common public-dependency view, independently of the current repository.                                                              |

Without the flag, a private override affects only the selected workspace. Host
scope affects only common retention, not a host-wide private default or a bulk
update of private views. Neither scope inherits the other's override.
[CLI Retention Commands](cli.md#cache-retention-commands) owns parsing/help.

Both private and common policies must be finite, strictly positive and exactly
representable by the native backend. Human durations still use `humantime` with
checked domain/precision/range conversion. Zero-duration/native
disabled-eviction values, permanent retention and overflow/truncation are not
supported; reject them before state mutation or backend dispatch. This does not
change the separate GC access-wait contract, where a parsed zero means an
immediate attempt.

Bare information reports selected scope, configured effective duration and
built-in-default/explicit-override source. It does not create policy/cache
state, register/provision views or services, change lifecycle or assert that the
configured policy is active.

Set saves a persistent override for that scope. Reset removes only its override
so that scope's built-in default applies, rather than storing an explicit
replacement default. Overrides survive later invocations and service
replacement. Private views default to 30 days of inactivity before GC
eligibility, measured from last access or initial publication when never
accessed. Access refreshes that age; private reset restores this default by
removing only its override. Eligibility does not promise deletion at an exact
deadline. Private policy stays finite under
[Cache Eviction](#cache-commands-locking-and-retention); the independent common
default is owned by [Common Retention](#common-retention). For example,
`set 90d` changes this workspace only; `set 90d --host` changes common only.
Reset never wipes data, rotates keys or shuts down services.

For an active selected view on a running compatible backend, successful
set/reset means the policy is durably saved and confirmed active before return.
Application does not restart/replace shared infrastructure or change other
views' policies, namespaces or grants. When the selected view/backend is
positively known absent or stopped, save for ordinary provisioning/startup
without initiating it; report saved-for-startup, not active. Ordinary startup
applies that view's saved policy before reporting it ready or collecting under
an obsolete policy.

Persistence, application or confirmation failure returns nonzero and reports
known saved/active partial effects or uncertainty. Inaccessible or incompatible
running-backend access is not successful activation or proof of stopped state.
No rollback or no-change promise is implied. Setters do not repair, restart or
replace infrastructure to force application.

Both policies are controlled by the host user, not repository `[cache]` settings
or workspace environment. Ordinary startup may apply saved policy, but cannot
silently overwrite/reset it. Scope flags select existing management authority;
they do not grant host-policy authority to agent-facing endpoints.

### Common Retention

Acceptance: [Retention Acceptance](#retention-acceptance).

The common public-dependency view defaults to 30 days of inactivity before an
entry becomes GC-eligible, measured from last access or initial publication when
never accessed. Access refreshes age. Eligibility does not promise deletion at
an exact deadline. Its independent persistent override and reset follow
[Retention Control](#retention-control); host reset restores 30 days.

Common entries are not pinned indefinitely by a registered project's roots.
After expiry, a returning project may need upstream substitution or rebuilding;
offline availability is not guaranteed. Private entries are independently
age-evictable under [Cache Eviction](#cache-commands-locking-and-retention).

Expiry removes a view entry, not another view's unexpired membership. Physical
data retained by a private/common view or native in-flight reference survives.
Age cleanup is not a hard disk cap, key rotation, namespace shutdown or service
idle timer.

### Remote builders

Direct remote-builder access to the local project cache is out of scope for v1.
Remote builders may build normally; after outputs are available in the host
store, host-side publishing can publish eligible outputs into the local project
cache for later sandbox substitution. Wrix does not bind the project cache to
LAN/VPN addresses or configure remote builder trust for the local cache by
default.

### Signing and Repair

Acceptance: [Signing Acceptance](#signing-acceptance).

Celler signs Nix binary-cache responses under the selected authorized view's
key. Successful upload alone is not evidence of usable signed publication: the
production HTTP path must substitute under that key with signature requirements
enabled. The host publisher uses scoped upload authority, not a sandbox-provided
cache name, signing key or destination path. Signing state belongs to Celler's
managed service data and is never delivered to agent sandboxes.

Eligible entries supplied without uploader signatures receive usable server
signing under the existing valid cache key. Publication/repair remains scoped
and serialized under the namespace cache lock; it neither rotates a valid key,
broadens root eligibility nor authorizes worker promotion. Server-side signing
may supply the signature on read; storing an uploader signature is not required.

Wrix-generated host cache configuration must not admit sandbox-upload views or
their signing keys. Direct worker uploads must not enter host-trusted common or
private publication merely because the repository matches or Celler signs the
bytes: server acceptance does not attest correct derivation execution. Any
future worker-upload feature must preserve separate view/key trust from host
publication; independent trusted host builds of the same inputs remain eligible
normally.

Cohosted compiler/procedural-macro execution can affect Celler signing/backend
state without worker uploads or a sandbox escape. This can corrupt sandbox
substitution and service-visible data; signatures are not build correctness.
Excluding host consumption prevents Wrix-managed promotion of that authority
into host software trust. Runtime escapes, host-client vulnerabilities and later
execution of agent-modified workspace files remain separate risks. Unmounted
issuer/publishing files do not isolate service-held signing keys.

Acceptance uses disposable caches/keys and input-addressed derivation outputs,
not content-addressed paths that Nix can legitimately trust without a signature.
A fresh isolated client uses only the production test HTTP endpoint and selected
public key with `require-sigs=true`: it cannot pass through upstream fallback or
an already-local output. A reachable unsigned HTTP/NAR negative control is
rejected. An eligible already-present unsigned entry becomes usable under the
valid unchanged key through the Celler publication/signing path.

## Success Criteria

### Celler Backend Acceptance

- The packaged shared service uses Celler alongside Dolt, proxy and shared
  sccache, without a native host Celler daemon or flat-file cache fallback.
  [system?](test-ci:test-cache-celler-system)
- Host publication reuses Celler's pure-Rust store client for metadata queries
  and NAR streaming; Nix CLI root discovery and flake evaluation remain allowed.
  [system](test-ci:test-cache-celler-native-system)
- Host integration publishes through authenticated Celler HTTP without adding
  shared-cache substituters, signing-key trust or pull credentials to host Nix.
  [system?](verify:cache.celler-host-integration)
- Live clients using ordinary cache API credentials can read authorized
  metadata/archives but cannot read unauthorized peer views or mutate,
  administer or publish through that interface.
  [system?](test-ci:test-cache-celler-tenancy)
- Identical archives can share backing storage while a guessed cache name or
  private NAR hash does not grant another workspace access or membership.
  [system?](test-ci:test-cache-celler-tenancy)
- Namespace stop, host-wide collection and private-key rotation preserve
  unrelated views/keys and archives still held by an unexpired view or native
  in-flight reference. [system?](test-ci:test-cache-celler-tenancy)
- Container replacement preserves Celler metadata, archives and valid keys;
  namespace registration does not replace the container or broaden its mounts.
  [system?](test-ci:test-cache-celler-state)
- Direct worker mounts and cache APIs expose no managed signing state; the
  shared service holds it under the accepted compiler trust boundary, while host
  publishing/issuer credential files and manifests remain unmounted.
  [system?](test-ci:test-cache-celler-credential-isolation)
- Runtime pull credentials and publishing/issuer secrets are absent from Nix
  outputs, hook snapshots, image layers and preflight/status diagnostics.
  [system?](test-ci:test-cache-celler-credential-isolation)
- Each authorized destination receives only its explicitly selected candidate
  paths under scoped host publishing authority; the backend does not recursively
  expand that set. [system](test-ci:test-cache-celler-native-system)
- Celler integration works with the existing sandbox runtime contracts without
  requiring containerd/nix-snapshotter, shared worker execution or a host-store
  mount. [system](test-ci:test-cache-celler-native-system)

### Visibility Acceptance

- Two unrelated repositories can substitute an approved common dependency
  through authenticated native Nix without duplicate backing archives solely
  because the repositories differ. [system?](test-ci:test-cache-celler-sharing)
- Private views follow canonical workspace identity, including shared identity
  across Loom clones of the same repository.
  [test](../crates/wrix-cache/src/backend/issuer/test/pull/mod.rs::canonical_loom_clones_share_views_but_unrelated_and_nonrepository_paths_do_not)
- Ordinary cache requests from unrelated repositories cannot read private-only
  metadata or archives, even using known paths, hashes or direct archive URLs.
  [system?](test-ci:test-cache-celler-sharing)
- Common reads require authentication, and physical deduplication or an
  agent-supplied public flag cannot expose private-only contents there.
  [system?](test-ci:test-cache-celler-sharing)
- Repository/agent input and supplied cache names cannot broaden the exact
  common-plus-own-private read set or enroll an additional audience.
  [test?](../crates/wrix-cache/tests/celler_sharing.rs::read_authority_is_common_and_own_private_only)
- Native Nix receives only authorized endpoints, credentials and public keys,
  preserving signatures and excluding agent publishing/management authority.
  [system?](test-ci:test-cache-celler-sharing)
- Namespace stop and private-key rotation preserve the common view and unrelated
  private views rather than treating deduplication as deletion authority.
  [system?](test-ci:test-cache-celler-sharing)
- Private/custom outputs cannot be made common merely through read access or
  agent/repository audience requests.
  [test?](../crates/wrix-cache/tests/celler_population.rs::read_membership_and_repository_input_cannot_broaden_publication)

### Pull Credential Acceptance

- Host-generated Nix configuration excludes every shared-service cache view and
  signing key, independent of repository identity or server signatures.
  [test?](../crates/wrix-cache/tests/celler_auth.rs::host_trust_excludes_all_shared_service_views_and_keys)

- Sandbox pull tokens have a one-day nominal lifetime from issuance, rather than
  adopting an unbounded or silently extended validity period.
  [test](../crates/wrix-cache/src/backend/issuer/test/pull/mod.rs::sandbox_pull_credentials_are_exact_read_only_and_expire_after_one_day)
- Issued credentials grant only exact named views and pull access; views remain
  authenticated and JWT issuer private authority stays host-side, distinct from
  backend Nix signing state. [system](test-ci:test-cache-celler-native-system)
- Renewal supplies credentials from current host authorization, not stale token
  grants or agent/repository input after authorization withdrawal.
  [test](../crates/wrix-service/src/control/cache/test/mod.rs::renewal_uses_current_host_policy_and_never_replays_withdrawn_audiences)
- Production Celler accepts a valid scoped native token and rejects expired or
  tampered credentials under its native verification rules.
  [system](test-ci:test-cache-celler-native-system)
- Withdrawal of view authorization does not revoke an already-valid token: newly
  published contents remain readable until native expiry, while renewed
  credentials omit withdrawn access and preserve remaining authorized views.
  [system](test-ci:test-cache-celler-native-system)
- The documented authorization model uses native expiry rather than custom
  membership lookups/denylists or active-download cancellation, acknowledges
  bearer reuse and native verification timing, and distinguishes JWT
  verification rotation from Nix signing-key rotation.
  [judge?](../tests/judges/cache-auth-lifecycle.md#native-expiry)

### Host Consumption Exclusion Acceptance

- Enabled host activation and managed builds publish without injecting shared
  substituters, shared key trust, pull tokens or host freshness settings.
  [test?](../crates/wrix-cache/tests/host_publication_only.rs::host_configuration_is_publication_only)
- First and actual cached direnv activation admit publication without minting
  host pull credentials or clobbering unrelated native credentials.
  [system?](test-ci:test-cache-host-publication-only)
- Known effective inherited shared-cache substituters/keys block enabled host
  integration with actionable guidance, without rewriting operator settings,
  deleting imported outputs or claiming their recall.
  [test?](../crates/wrix-cache/tests/host_publication_only.rs::inherited_shared_cache_trust_requires_explicit_remediation)
- Publication responses cannot authorize host substitution, public
  classification, code execution or build/check success.
  [test?](../crates/wrix-cache/tests/host_publication_only.rs::backend_responses_do_not_grant_host_trust)
- A disposable signed malicious shared-cache output substitutes for its sandbox
  positive control but not through production host configuration; independent
  upstream and local-store reuse remain functional.
  [system?](test-ci:test-cache-host-consumption-exclusion)
- Sandbox launch issues its own one-day credential independently of host
  publication, without host pull-token inheritance.
  [test?](../crates/wrix-sandbox/tests/cache_credentials.rs::sandbox_pull_issuance_is_independent_of_host_publication)

### Launcher Renewal Acceptance

- A waiting host launcher renews before expiry across multiple token lifetimes
  without requiring an agent/container restart or a separate renewal daemon.
  [test?](../crates/wrix-sandbox/tests/cache_credentials.rs::launcher_renews_across_multiple_token_lifetimes)
- Replacement credentials are complete and isolated per launch, delivered by
  atomic replacement inside a private read-only directory mount via stable
  native `netrc-file`, not a frozen token environment value/file snapshot.
  [test?](../crates/wrix-sandbox/tests/cache_credentials.rs::credential_refresh_is_atomic_and_per_launch)
- Renewal failures retry/report without secrets, agent-protocol stdout
  pollution, lifecycle disruption, broader grants or artificial extension of old
  validity.
  [test?](../crates/wrix-sandbox/tests/cache_credentials.rs::renewal_failures_preserve_protocol_lifecycle_and_authority)
- Suspension/clock-change recovery rechecks absolute expiry rather than relying
  only on elapsed awake time, without promising zero authentication
  interruption.
  [test?](../crates/wrix-sandbox/tests/cache_credentials.rs::resume_rechecks_absolute_token_expiry)
- Normal completion stops renewal and cleans private staging; launcher exit does
  not leave a detached renewer or introduce early revocation of issued tokens.
  [test?](../crates/wrix-sandbox/tests/cache_credentials.rs::launcher_completion_stops_renewal_and_cleans_credentials)
- Real Podman/native Nix clients observe replaced credentials and continue
  authorized substitution without restart; the agent cannot modify the mount.
  [system?](test-ci:test-cache-renewal-podman)
- Real krun/native Nix clients observe replaced credentials and continue
  authorized substitution without restart; the agent cannot modify the mount.
  [system?](test-ci:test-cache-renewal-krun)
- Real Apple/native Nix clients observe replaced credentials and continue
  authorized substitution without restart; the agent cannot modify the mount.
  [system?](test-ci:test-cache-renewal-apple)

### Population Acceptance

- Approved public-source authority starts with cache.nixos.org and its trusted
  key; ambient substituters and repository/agent input cannot approve additional
  public sources.
  [test?](../crates/wrix-cache/tests/celler_population.rs::public_source_authority_is_independent_of_repository_input)
- Eligible realized paths with verified public identity route to common despite
  upstream availability; remaining candidates retain private upstream filtering.
  [test?](../crates/wrix-cache/tests/celler_population.rs::verified_public_hits_route_to_common_before_private_filtering)
- Invalid signatures, unknown keys, path/hash mismatches and missing/malformed
  or inconclusive evidence never authorize common publication.
  [test?](../crates/wrix-cache/tests/celler_population.rs::unverified_or_mismatched_identity_cannot_become_common)
- Public population considers only realized eligible-closure paths, without a
  whole-store scan, builds or downloads of missing outputs to seed common.
  [test?](../crates/wrix-cache/tests/celler_population.rs::public_population_is_realized_only_and_project_scoped)
- Uploaded NAR bytes that do not match the verified public identity cannot
  become a usable common entry.
  [test?](../crates/wrix-cache/tests/celler_population.rs::upload_bytes_must_match_verified_public_identity)
- Common publication uses public/safe metadata without disclosing a local-only
  deriver or private build/source metadata attached to an otherwise public NAR.
  [test?](../crates/wrix-cache/tests/celler_population.rs::common_metadata_excludes_private_host_details)
- Canonical completion, scoped hook publication and explicit publish/warm use
  the same verified routing without automatic warming or discovery outside the
  admitted selected/explicit scope.
  [test?](../crates/wrix-cache/tests/celler_population.rs::publication_entrypoints_share_verified_routing)
- A signed upstream fixture's matching eligible host output substitutes for a
  sandbox client from Celler common for a second repository's fresh isolated
  native Nix client with require-sigs enabled and no upstream/local shortcut;
  private/mismatched output remains unavailable through common.
  [system?](test-ci:test-cache-celler-public-population)
- Concurrent host publication of the same verified public object remains usable
  without corrupting peer views or broadening their permissions.
  [system?](test-ci:test-cache-celler-public-population)
- Sandbox cache misses do not activate service-side upstream fetching, and
  sandbox publication remains denied.
  [system?](test-ci:test-cache-celler-public-population)

### Build Pipeline Acceptance

- Production host verifier/CI realization, managed image preparation and cache
  warming dispatch through the shared pipeline without requiring duplicate
  cache-root declarations. [system?](test-ci:test-cache-host-build-pipeline)
- Selected CI runners and realized build-only dependency outputs are registered
  from the actual graph, and graph changes update registration without manual
  root-list maintenance.
  [test?](../crates/wrix-cache/tests/build_pipeline.rs::selected_graph_registration_tracks_realized_build_dependencies)
- Cache disablement and applicable explicit publication exclusions remain
  authoritative for automatically selected builds.
  [test?](../crates/wrix-cache/tests/build_pipeline.rs::selected_builds_respect_cache_opt_out_and_exclusions)
- Root exclusions prevent matching direct root admission without subtracting
  their realized outputs when reached through another admitted graph; candidate
  discovery does not evaluate excluded roots to apply a transitive denylist.
  [test?](../crates/wrix-cache/tests/build_pipeline.rs::root_exclusions_filter_admission_not_transitive_candidates)
- Unrelated host paths and unrealized outputs are not harvested, and missing
  outputs are not built merely to seed the cache.
  [test?](../crates/wrix-cache/tests/build_pipeline.rs::graph_publication_is_selected_and_realized_only)
- Substitution-only success reconciles and publishes eligible realized outputs
  through the production pipeline even when no post-build hook fired.
  [system?](test-ci:test-cache-host-build-pipeline)
- A selected runner's eligible build-only artifacts are usable from production
  signed HTTP cache by a fresh authorized client without upstream/local
  shortcuts. [system?](test-ci:test-cache-host-build-pipeline)
- Scoped graph discovery/publication does not evaluate all configured roots or
  realize unselected browser/image closures.
  [system?](test-ci:test-cache-host-build-pipeline)
- Publication registrations authorize selected scope without creating permanent
  cache pins or changing native eviction when a graph changes.
  [test?](../crates/wrix-cache/tests/build_pipeline.rs::graph_registration_is_admission_not_retention)

- Concurrent graph/configured-root updates preserve other admitted active scopes
  and pending work without a cache-retention claim registry.
  [test?](../crates/wrix-cache/tests/build_pipeline.rs::concurrent_registrations_preserve_independent_admission_scopes)
- Build/publication failures report actual pending/error state and do not
  trigger deletion/pruning of prior cache data; independent age eviction remains
  native.
  [test?](../crates/wrix-cache/tests/build_pipeline.rs::failed_publication_reports_truthfully_without_pruning)
- In-flight publication outputs survive competing Nix GC through actual store
  roots, exercised only against disposable isolated stores/daemons.
  [system?](test-ci:test-cache-host-build-pipeline)

- Canonical host dispatch does not enroll guest-local builds, deliver host
  publishing credentials to agents or treat arbitrary Cargo commands as Nix
  publication events.
  [test?](../crates/wrix-cache/tests/build_pipeline.rs::canonical_coordination_is_host_nix_only)

### Host Entry Publication Acceptance

- Activation admits only the already-realized selected devshell environment and
  dependencies, without unrelated root discovery or extra project/platform
  builds.
  [test?](../crates/wrix-cache/tests/build_pipeline.rs::activation_admits_realized_selected_devshell_only)
- First real host activation publishes eligible dependencies realized before
  hook setup; a fresh authorized native client can substitute them through
  signed Celler HTTP without upstream/local-store shortcuts.
  [system?](test-ci:test-cache-devshell-publication)
- Actual unchanged/cached direnv activation reconciles eligible environment
  paths even without a new build event or manual publication refresh.
  [system?](test-ci:test-cache-devshell-publication)
- Enabled/disabled and excluded activation paths retain cache policy and
  platform-specific selection instead of broadening publication or starting
  cross-platform builds.
  [test?](../crates/wrix-cache/tests/build_pipeline.rs::activation_respects_cache_policy_and_platform_scope)
- A real host push through installed hooks outside a devshell publishes its
  eligible realized Nix check/runner/build-only outputs for fresh signed HTTP
  use. [system?](test-ci:test-cache-pre-push-publication)
- Substitution-only host pre-push realization still reconciles eligible outputs
  without relying on a post-build hook event.
  [system?](test-ci:test-cache-pre-push-publication)
- Host entry-point publication uses automatic pending/error handling without
  granting worker publication authority or turning upload results into check
  verdicts.
  [test?](../crates/wrix-cache/tests/build_pipeline.rs::host_entry_publication_preserves_caller_and_authority_rules)

### Freshness Acceptance

- Enabled sandbox run/spawn Nix configuration supplies an effective zero
  negative-cache TTL without disabling signatures or modifying host contexts.
  [test?](../crates/wrix-cache/tests/celler_freshness.rs::sandbox_contexts_use_zero_negative_ttl)
- Freshness stays context-scoped without machine-wide configuration/client
  database edits; opt-out does not add/reset it solely for Wrix integration.
  [test?](../crates/wrix-cache/tests/celler_freshness.rs::freshness_configuration_is_context_scoped_and_respects_opt_out)
- Existing run/spawn sandboxes substitute a previously missed signed
  input-addressed output on the next ordinary command after host publication,
  without restart, manual refresh, local/upstream shortcuts or changed
  endpoints. [system?](test-ci:test-cache-publication-freshness-sandbox)
- Sandbox zero negative TTL preserves positive metadata-cache hits.
  [system?](test-ci:test-cache-publication-freshness-sandbox)
- Valid sandbox store paths remain reusable without forced deletion/reimport.
  [system?](test-ci:test-cache-publication-freshness-sandbox)
- Sandbox repeated-miss measurements record upstream request counts/time against
  ordinary negative caching separately from positive/local hits, without
  promising zero overhead or an advance speedup.
  [system?](test-ci:test-cache-publication-freshness-sandbox)

### Retention Acceptance

- Private/common retention accepts only finite positive native-representable
  durations, rejecting zero/disabled eviction, permanent retention, overflow or
  lossy conversion before mutation/dispatch without changing GC-wait zero
  semantics.
  [test?](../crates/wrix-cache/tests/celler_retention.rs::retention_requires_finite_positive_native_representable_duration)

- Retention information reports selected private/common scope, configured
  duration and default/override source without creating or mutating
  policy/cache/services.
  [test?](../crates/wrix-cache/tests/celler_retention.rs::retention_information_reports_selected_scope_without_mutation)
- Private information/set/reset use [Workspace Admission](workspaces.md) scope,
  without changing peers or common policy.
  [test?](../crates/wrix-cache/tests/celler_retention.rs::private_retention_uses_canonical_workspace_scope)
- Host information/set/reset select common policy without repository dependence,
  private-view inheritance or a bulk private-policy update.
  [test?](../crates/wrix-cache/tests/celler_retention.rs::host_retention_selects_only_common_policy)
- Private entries without an override become GC-eligible after 30 days from last
  access, or initial publication when never accessed.
  [test?](../crates/wrix-cache/tests/celler_retention.rs::private_default_is_thirty_days_of_inactivity)
- Private-entry access refreshes age rather than retaining the original
  publication or last collection time as the inactivity baseline.
  [test?](../crates/wrix-cache/tests/celler_retention.rs::private_access_refreshes_retention_age)
- Each selected scope's override persists across invocations/service
  replacement; reset removes only that override and restores its 30-day built-in
  default.
  [test?](../crates/wrix-cache/tests/celler_retention.rs::scoped_overrides_persist_and_reset_independently)
- Reset changes policy only, without wiping data, rotating keys or stopping
  services.
  [test?](../crates/wrix-cache/tests/celler_retention.rs::retention_reset_clears_only_selected_scope_override)
- Set/reset for an active selected view on a compatible backend succeed only
  after the resulting policy is both durably saved and confirmed active.
  [test?](../crates/wrix-cache/tests/celler_retention.rs::running_updates_require_saved_and_confirmed_active_policy)
- With the selected view/backend known absent/stopped, set/reset save for
  ordinary provisioning/startup without initiating registration, provisioning or
  repair.
  [test?](../crates/wrix-cache/tests/celler_retention.rs::stopped_updates_save_without_service_startup)
- Ordinary startup applies saved retention before reporting cache readiness or
  allowing collection of the selected view under an obsolete policy.
  [test?](../crates/wrix-cache/tests/celler_retention.rs::startup_applies_saved_retention_before_readiness_and_collection)
- Save/apply/confirmation failures return nonzero and report known saved/active
  partial state or uncertainty without promising rollback or treating
  unavailable running-backend access as successful activation.
  [test?](../crates/wrix-cache/tests/celler_retention.rs::update_failures_report_partial_state_and_uncertainty)
- Live private/common set/reset changes only the selected view's active
  retention used by production collection, without changing other policies,
  restarting shared services or disrupting peer namespaces/grants.
  [system?](test-ci:test-cache-celler-retention)
- Without a host override, common entries become GC-eligible after 30 days from
  their last access, or initial publication when never accessed.
  [test?](../crates/wrix-cache/tests/celler_retention.rs::common_default_is_thirty_days_of_inactivity)
- Access refreshes common-entry age so eligibility tracks current use rather
  than the original publication or last collection time.
  [test?](../crates/wrix-cache/tests/celler_retention.rs::common_access_refreshes_retention_age)
- Private/common overrides are host-controlled: repository input and ordinary
  startup/environment cannot overwrite/reset them or confer management
  authority.
  [test?](../crates/wrix-cache/tests/celler_retention.rs::retention_overrides_are_host_controlled_not_workspace_input)
- Expired private and common entries are collectible even while their target
  remains configured/current; cache expiry does not delete installed client
  paths. [system?](test-ci:test-cache-celler-retention)
- Expiring one view preserves physical data held by an unexpired private/common
  view or a native in-flight reference.
  [system?](test-ci:test-cache-celler-retention)
- Common expiry does not rotate keys, retire namespaces or stop the shared
  service, and is not treated as a hard size cap.
  [system?](test-ci:test-cache-celler-retention)

### Cache Acceptance

- Bare cache information reports the selected workspace's effective policy and
  recorded publication/pending state, excludes secrets, and distinguishes
  stale/unavailable metadata from live readiness or current artifact retention.
  [test?](../crates/wrix-cli/tests/information_defaults.rs::bare_cache_reports_policy_and_recorded_publication_state)
- Bare cache information remains passive for absent, unregistered or stopped
  infrastructure: no managed-state creation, registration, startup/repair,
  credential issuance, SQL/backend probes, publication, collection or pending
  expiration.
  [test?](../crates/wrix-cli/tests/information_defaults.rs::bare_cache_information_is_passive_even_when_unregistered_or_stopped)

- Devshells export only explicit cache overrides and preserve explicit resets.
  [check](verify:services.cache-devshell-overrides)

- Cache commands read repository policy outside direnv without mutation.
  [test](../crates/wrix-cli/tests/cache_config.rs::cache_config_reads_repository_policy_outside_direnv_without_mutation)

- Repository cache opt-out suppresses cache-only startup; environment overrides
  and explicit service flags take precedence.
  [test](../crates/wrix-cli/tests/service_cache.rs::repository_cache_opt_out_suppresses_startup_and_environment_can_override_it)

- Automatic publishing uses immutable resolved settings without consulting
  repository policy or ambient cache configuration.
  [test](../crates/wrix-cache/tests/publisher.rs::daemon_publisher_uses_snapshot_without_ambient_config_or_repository_reads)

- Warm reads repository check policy, while `--checks` overrides environment.
  [test](../crates/wrix-cache/tests/publisher.rs::warm_reads_repository_policy_and_checks_flag_overrides_environment)

- Explicit environment values, including built-in defaults and empty lists,
  override repository cache policy.
  [test](../crates/wrix-cache/tests/config.rs::environment_can_reset_repository_policy_to_builtin_values_and_empty_lists)

- Cache policy resolves from the invoked repository independently of shared Loom
  service identity.
  [test](../crates/wrix-cache/tests/config.rs::cache_policy_reader_uses_nearest_repository_not_shared_loom_service_identity)

- Repository pending TTL controls expiration outside direnv.
  [test](../crates/wrix-cache/tests/publisher.rs::repository_pending_ttl_expires_records_without_direnv)

- Publish, warm, graph refresh and workspace startup do not dispatch Celler GC
  or root-reachability pruning; native backend scheduling remains host-managed.
  [test?](../crates/wrix-cache/tests/celler_retention.rs::workspace_and_publication_paths_do_not_trigger_collection)

- Project cache public keys parse as a safe name and a base64-encoded 32-byte
  Ed25519 key; padded and unpadded keys remain supported, while malformed
  encodings and configuration injection fail before launcher trust rendering
  [test](../crates/wrix-sandbox/src/command/launch.rs::launcher_cache_key_reader_rejects_malformed_encoding_and_config_injection)

- Namespace registration preserves valid Celler keys; invalid-key repair does
  not replace valid peer keys, and failed key generation preserves prior state.
  [test](../crates/wrix-cache/tests/celler_backend.rs::key_registration_and_failed_repair_preserve_valid_state)

- Nix packaging exposes Wrix hook/publisher entry points and the service's
  Celler backend/read admission, without requiring the retired static-server
  helper. [check](test-ci:test-cache-celler-packaging)

- Enabled cache integration separates Linux XDG/Darwin Library host-private
  workspace control state from managed Celler metadata/signing/archive data
  outside worktrees; opt-out creates no state solely for cache use.
  [test](../crates/wrix-cli/tests/service_cache.rs::celler_layout_separates_managed_cache_data_from_host_authority)

- Host devshell Nix installs the immutable publication hook without shared-cache
  substitution/key trust and fails loudly when required publication settings are
  ignored. [system?](verify:cache.celler-host-integration)

- Workspace state retains GC roots for the installed post-build wrapper and its
  hook, publisher and interpreter store outputs while the workspace is active.
  [system](verify:services.host-nix-config)

- Host integration rejects a conflicting non-Wrix post-build hook instead of
  replacing or chaining it. [system](verify:services.host-nix-config)

- Shell entry reconciles only the realized selected environment rather than
  enumerating all configured publish roots; supplementary-manifest reminders do
  not require manual refresh before automatic activation publication.
  [system?](test-ci:test-cache-devshell-publication)

- Both launch modes configure real sandbox Nix for authorized Celler
  endpoint(s), matching public keys, scoped runtime pull authorization and
  `builders-use-substitutes`, without depending on container DNS.
  [system?](test-ci:test-cache-celler-system)

- The Celler-facing read-admission path permits only scoped binary-cache
  GET/HEAD resources and rejects mutation/admin, listing and traversal routes.
  [test?](../crates/wrix-cache/tests/celler_frontend.rs::read_routes_are_scoped_and_read_only)

- Idle clients cannot block independent production cache requests.
  [test?](../crates/wrix-cache/tests/celler_frontend.rs::celler_production_idle_clients_do_not_block_other_requests)

- Disconnected/reset clients cannot terminate production read admission.
  [test?](../crates/wrix-cache/tests/celler_frontend.rs::celler_production_disconnected_clients_do_not_terminate_service)

- Read admission enforces the 32-request limit and complete headers within 8
  KiB, 32 fields and five seconds.
  [test?](../crates/wrix-cache/tests/celler_frontend.rs::request_admission_and_headers_are_bounded)

- Trickle-fed headers cannot extend the five-second request deadline.
  [test?](../crates/wrix-cache/tests/celler_frontend.rs::celler_production_partial_request_deadline_is_not_extended_by_trickling)

- Production response writes obey 30-second inactivity and five-minute total
  deadlines.
  [test?](../crates/wrix-cache/tests/celler_frontend.rs::stalled_body_writes_obey_response_deadlines)

- HEAD does not read archive bodies and large GET responses stream within
  bounded memory.
  [test?](../crates/wrix-cache/tests/celler_frontend.rs::head_avoids_archive_bodies_and_get_streams)

- Sandboxes receive no cache signing key, no durable state root mount, no host
  `/nix/store` mount, and no host Nix daemon socket as part of project-cache
  integration [system?](test-ci:test-cache-celler-credential-isolation)

- Restricted sandbox Nix can reach its scoped Celler read endpoint while
  publication/management and unrelated host-local endpoints remain blocked.
  [system?](test-ci:test-cache-celler-system)

- The post-build hook drops privileges before publishing, never executes
  workspace files, and admits only registered selected-graph members or
  configured supplementary roots, not arbitrary devshell builds.
  [test?](../crates/wrix-cache/tests/hook.rs::post_build_hook_scopes_publish_to_registered_graphs)

- A failed publisher ownership switch prevents any publisher execution
  [test](../crates/wrix-cache/tests/hook.rs::post_build_hook_never_publishes_when_owner_switch_fails)

- Standalone publish refreshes configured publication admission, drains matching
  pending work and publishes realized paths without builds, GC or overwriting
  canonical graph registrations.
  [test?](../crates/wrix-cache/tests/build_pipeline.rs::explicit_publish_preserves_canonical_graph_registrations)

- Warm builds packages/devShell through the shared pipeline, honors check
  selection and publication policy, and does not collect cache entries.
  [test?](../crates/wrix-cache/tests/build_pipeline.rs::warm_uses_pipeline_without_collection)

- Repository-private publication admits only selected graph or configured-root
  candidates, subtracts configured upstream hits and never harvests arbitrary
  host paths.
  [test?](../crates/wrix-cache/tests/build_pipeline.rs::private_publication_uses_admitted_scope_and_upstream_filtering)

- Lock contention in the automatic publisher prints a waiting message; lock
  timeout records durable pending work and a warning, and a later explicit
  publish drains matching pending records
  [test](../crates/wrix-cache/tests/publisher.rs::lock_timeout_records_pending_and_explicit_publish_drains)

- Configured-root refresh updates publication admission without erasing
  canonical scopes/pending work or evicting objects by root reachability.
  [test?](../crates/wrix-cache/tests/build_pipeline.rs::configured_root_refresh_preserves_admission_without_eviction)

- Warming a subset preserves other configured/canonical publication scopes while
  respecting exclusions, without creating private retention pins.
  [test?](../crates/wrix-cache/tests/build_pipeline.rs::warm_subsets_preserve_other_admission_scopes)

- Obsolete repository pruning controls are rejected, and workspace environment
  cannot override host-managed Celler collection scheduling.
  [test?](../crates/wrix-cache/tests/celler_retention.rs::retired_pruning_controls_cannot_schedule_collection)
- Nix cache adapters reject the retired pruning option instead of exporting it
  as a workspace policy override. [check?](verify:cache.celler-policy-adapter)
- Private-view provisioning configures finite native age eligibility rather than
  inheriting disabled eviction or current-target retention pins.
  [test?](../crates/wrix-cache/tests/celler_retention.rs::private_views_require_finite_native_eviction)

- Key rotation invalidates only the selected Celler view, replaces its key and
  metadata, and requires republishing without old/new trust overlap or peer
  loss.
  [test](../crates/wrix-cache/tests/celler_backend.rs::rotation_invalidates_only_the_selected_view)

- `wrix service cache status` reports cache size and warns above the default 50
  GiB soft threshold without deleting reachable entries solely to satisfy that
  threshold
  [test](../crates/wrix-cache/tests/publisher.rs::status_warns_above_soft_size_without_pruning)

- Cache status reports namespace size, pending work, publication outcomes/errors
  and endpoints without mutation or invented workspace-GC results.
  [test?](../crates/wrix-cache/tests/celler_retention.rs::status_reports_publication_without_workspace_prune_state)

- The container-facing cache transport is HTTP only; wrix does not configure
  Unix-socket cache substituters, host Nix daemon sockets, shared mutable
  `/nix/store`, or host-store-serving tools for sandbox cache reads
  [check](verify:services.cache-transport-http-only)

- Upstream queries batch at most 256 paths, retaining null misses as publishable
  candidates.
  [test](../crates/wrix-cache/tests/publisher.rs::publish_batches_upstream_queries_and_retains_null_misses)

- Successful upstream-query entries survive partial query failure without
  treating unknown paths as upstream hits.
  [test](../crates/wrix-cache/tests/publisher.rs::publish_uses_successful_upstream_entries_even_on_partial_query_failure)

- Omitted cache policy uses the documented enable/trust, publish/warm root,
  size, pending-TTL and lock defaults.
  [test](../crates/wrix-cache/tests/config.rs::omitted_cache_policy_uses_documented_defaults)
- Repository cache policy overrides only supplied fields, retaining other
  defaults.
  [test](../crates/wrix-cache/tests/config.rs::repository_cache_policy_overrides_only_specified_fields)
- Numeric cache quantities retain byte/second units and supported byte-size
  spellings.
  [test](../crates/wrix-core/tests/repository_policy.rs::cache_quantities_accept_documented_units_and_plain_numbers)
- Human-readable cache duration strings use `humantime` with checked destination
  conversions, not the former hand-written suffix allowlist.
  [test](../crates/wrix-core/tests/repository_policy.rs::cache_duration_strings_use_humantime)
- Malformed cache quantities, overflowing sizes and wrong-typed root settings
  fail.
  [test](../crates/wrix-core/tests/repository_policy.rs::invalid_cache_quantities_and_wrong_typed_roots_are_rejected)
- Unknown cache policy keys and nested root-setting keys fail.
  [test](../crates/wrix-core/tests/repository_policy.rs::unknown_policy_sections_and_keys_are_rejected)
- Invalid explicit cache environment values fail instead of falling back.
  [test](../crates/wrix-cache/tests/config.rs::invalid_environment_overrides_fail_instead_of_falling_back)
- Lock durations that overflow milliseconds fail resolution.
  [test](../crates/wrix-cache/tests/config.rs::overflowing_lock_duration_fails_resolution)

### Signing Acceptance

- Documentation distinguishes one-way host publication from sandbox substitution
  and the accepted cohosted compiler boundary, including effects on Celler
  signing/sandbox integrity while private controller issuer files stay
  unmounted.
  [judge](../tests/judges/security-proxy-trust.md#shared-service-boundary)

- Publication selects the authorized Celler namespace and server signing,
  without granting clients private keys or using a flat-file destination.
  [system](test-ci:test-cache-celler-native-system)
- A newly published input-addressed output substitutes through the production
  Celler read endpoint into a fresh isolated sandbox-client store under its
  project public key and `require-sigs=true`, with no fallback or
  already-local-path shortcut.
  [system?](test-ci:test-cache-signed-http-substitution)
- An unsigned input-addressed negative control is reachable through working
  HTTP/NAR serving but is rejected by that same signature-checking client
  policy. [system?](test-ci:test-cache-signed-http-substitution)
- An eligible pre-existing unsigned entry becomes substitutable after
  publication/repair under the cache lock, without replacing a valid keypair.
  [system?](test-ci:test-cache-signed-http-substitution)

## Requirements

Acceptance: [Celler Backend Acceptance](#celler-backend-acceptance),
[Visibility Acceptance](#visibility-acceptance),
[Population Acceptance](#population-acceptance),
[Build Pipeline Acceptance](#build-pipeline-acceptance),
[Freshness Acceptance](#freshness-acceptance),
[Cache Acceptance](#cache-acceptance) and
[Signing Acceptance](#signing-acceptance).

### Functional

1. **Cache policy** — built-ins, repository policy, explicit environment and
   invocation overrides follow [Cache Policy](#cache-policy).
2. **Host publication** — scoped authenticated Celler upload, trusted hook setup
   and scoped automatic publishing follow
   [Host Nix Integration](#host-nix-integration-and-publish-hook).
3. **Sandbox pull** — read-only HTTP and public-key injection follow
   [Cache Transport](#cache-transport); hosting/endpoints belong to
   `services.md`.
4. **Publication and warming** — automatic selected-graph registration follows
   [Canonical Host Builds](#canonical-host-build-pipeline); supplementary roots,
   verified-public routing, private upstream subtraction, explicit warm and
   realized-only publish follow [Project Scope](#project-scope-and-root-sets),
   [Verified Public Population](#verified-public-population) and
   [Cache Commands](#cache-commands-locking-and-retention).
5. **Signed substitution** — Celler server signing and unsigned-entry handling
   follow [Signing and Repair](#signing-and-repair).
6. **Retention** — native age eviction and host-wide manual collection follow
   [Cache Commands](#cache-commands-locking-and-retention), with actual
   store-root protection under
   [Publication Scope](#publication-scope-and-in-flight-protection) and
   independent view overrides under [Retention Control](#retention-control).
7. **View authorization** — common dependencies and canonical private
   repositories follow [Cache Visibility](#cache-visibility); one-day sandbox
   bearer grants and expiry-based revocation follow
   [Native Pull Credentials](#native-pull-credentials). Host activation follows
   [Host Publication-only Integration](#host-publication-only-integration).
8. **Publication freshness** — next-command discovery of newly published paths
   and context-scoped zero negative TTL follow
   [Client Publication Freshness](#client-publication-freshness).
9. **Information default** — bare cache inspection follows
   [Cache Commands](#cache-commands-locking-and-retention) without starting or
   querying backends or changing state.

### Non-Functional

1. **Project scope** — publication/trust/retention follow
   [Cache Authority](#cache-authority) and
   [Cache Commands](#cache-commands-locking-and-retention); physical
   deduplication does not create an implicit private-view access/trust union.
   Intentional common reuse follows [Cache Visibility](#cache-visibility).
2. **Host-store isolation** — the transport boundary in
   [Cache Transport](#cache-transport) does not expose the host store or daemon.
3. **Local-first** — hosted caches may be upstreams, but are not required.
4. **Serving and retention** — request bounds, native age eviction and soft size
   warnings follow the contracts above; common expiry follows
   [Common Retention](#common-retention), not workspace pruning or service
   lifetime.

## Out of Scope

- Host-store-backed serving via Harmonia, nix-serve or equivalent host-store
  views.
- Alternative/parallel cache backends or legacy flat-file/static-server
  compatibility. Celler is the selected local backend; hosted services may be
  upstreams, not a required replacement backend.
- Host-native Celler deployment, dedicated host accounts or an additional cache
  VM. The shared service owns its lifecycle.
- Performance speedup guarantees and compression/chunk parameter commitments.
- Speculative changes to Celler's client error variants or invalid-path
  handling. Existing behavior is reused by default; a focused exception requires
  concrete evidence of an affected Wrix requirement and a narrowly justified
  change, not general error-model cleanup.
- Public classification from package names/provenance, build success, arbitrary
  configured substituters or agent/repository public flags instead of verified
  identity against host-approved public sources.
- Sharing groups, membership/target-sharing approval registries, group retention
  and automatic group-target publication. Group-management commands are
  deferred; canonical private views plus approved common dependencies bound this
  scope.
- Host shared-cache substitution/key trust, host pull-token refresh and
  automatic alteration of operator trust or previously imported outputs.
- Immediate token revocation, custom per-request policy lookups/denylists and
  active-download cancellation; use
  [Native Pull Credentials](#native-pull-credentials).
- Permanent retention or disabled eviction in either view, including native zero
  retention; this rollout is a finite age-evictable cache, not an archive.
- Indefinite current-target artifact retention, cache-specific root pin
  registries, transactional retention-claim generations and per-workspace Celler
  GC.
- Shared containerd/nix-snapshotter worker migration or moving full worker
  runtimes into the service VM (the explicitly delegated compiler lane is
  separate). The separate [Installed Store Plane](store.md) is not a Celler
  prerequisite or authorization for a dynamic dependency-mount broker.
- Service-side read-through population and fetching missing outputs solely to
  seed common; the host pipeline uses eligible outputs already realized locally.
- Direct remote-builder access; eligible outputs may publish after reaching the
  trusted host store.
- Sandboxed publication, automatic worker-output harvesting and host-validated
  promotion are deferred for the initial rollout. Sandboxes remain read-only
  cache consumers; no worker upload routes/credentials, publication hooks or
  sandbox-origin view/key are required. This avoids another untrusted write and
  credential/publication lifecycle while host activation/pre-push publication
  provides seeding. Guest-only build outputs are not shared through Celler.
- Shared mutable `/nix/store` volumes as a cache transport. Service-owned
  installed-store reuse belongs to [Service-Owned Nix Stores](store.md), not
  this cache contract. Celler itself does not preserve installed client state or
  remove fresh-client import costs.
- Non-Nix artifacts: Cargo targets/registry/git, sccache, virtualenvs and uv
  caches remain profile/tool-specific.
  [Shared/Private Tool Caches](profiles.md#shared-and-private-tool-cache-modes)
  do not authorize sandbox Nix publication or union Celler views.
- Automatic proactive warming or global host-store publication.
- Interception of arbitrary host Nix/Cargo commands, central execution of guest
  builds, a required new public build command, or an extra project-maintained
  cache-root list for Wrix-managed host builds.
- Host `/nix/store` collection by Wrix service GC or automatic broad image
  pruning as a consequence of build-graph registration.
- Cache-specific invalidation services, machine-wide Nix configuration changes,
  forced positive-cache refresh, interruption of running builds or retrofitting
  freshness configuration into previously launched containers.
