# Workspace Services

Per-workspace service container for shared local infrastructure used by the host devshell and wrix sandboxes: beads' Dolt server and the project-scoped Nix binary cache.

## Problem Statement

Wrix workspaces need long-lived local services that are shared by host commands and sandboxed containers without exposing the host filesystem, host `/nix/store`, or host Nix daemon to agents. The service lifecycle hosts Dolt and a project-scoped Nix binary cache so cold sandboxes can substitute project derivations instead of rebuilding them, while cache contents remain bounded to the current project.

## Architecture

`cli.md` owns the root `wrix` command grammar and dispatch. This spec owns the behavior behind `wrix service ...`: a per-workspace service container named `<repo>-service` plus the Dolt and project-cache operations exposed through that service group. The workspace identity is the canonical repository root when the current path is inside a Git checkout, including Loom-managed paths under `.loom/`; outside a checkout it falls back to the canonical current path. The human-readable container name uses the repository basename, while ports, state paths, labels, and locks derive from a hash of the identity path so multiple checkouts of the same repository do not collide.

The service container owns two service families:

- **Dolt for beads** — the Dolt SQL server for the Beads-owned workspace database. `beads.md` owns issue tracking and database storage; this spec owns only the shared container lifecycle and endpoint publication.
- **Project Nix cache** — a kiss-cache-compatible flat binary cache. Host Nix reads/writes the cache through the local filesystem; sandboxes read the same cache through a read-only HTTP server in the service container. The cache is an explicit project cache, not a host-store server: only paths published by wrix's project-scoped publishing rules are present.

### State layout

Mutable service state lives outside the git worktree except for the existing beads database. Wrix uses platform-native locations:

| Platform | Durable state root | Bulky cache root |
|----------|--------------------|------------------|
| Linux | `${XDG_STATE_HOME:-$HOME/.local/state}/wrix/workspaces/<workspace-hash>/` | `${XDG_CACHE_HOME:-$HOME/.cache}/wrix/workspaces/<workspace-hash>/binary-cache/` |
| Darwin | `$HOME/Library/Application Support/wrix/workspaces/<workspace-hash>/` | `$HOME/Library/Caches/wrix/workspaces/<workspace-hash>/binary-cache/` |

Durable state:

```text
<state-root>/
├── cache.lock              # publish/prune/rotate lock
├── cache-status.json       # dirty flag, last publish/prune status, last error
├── gcroots/                # retention markers per publish root
├── keys/
│   ├── cache.secret        # host-only signing key; never mounted into sandboxes
│   └── cache.pub           # public key distributed to host/sandbox Nix config
├── pending/                # deferred post-build candidates
│   └── <timestamp>-<pid>-<drv-hash>.json
├── publish-roots.json      # authoritative project-root manifest for publishing
└── services.json           # endpoint metadata, port leases, schema version
```

Bulky cache:

```text
<cache-root>/
├── nix-cache-info
├── *.narinfo
├── nar/
└── log/                    # optional
```

`beads.md` owns the Dolt database location and branch-sync state. The service container may mount that database and mounts `<cache-root>` read-only for HTTP serving. Sandboxes do not receive the durable state root, cache signing key, host `/nix/store`, host Nix daemon socket, or any authoritative cache publish manifest. Workspace-local `.wrix` may contain non-authoritative endpoint/debug pointers, but not cache signing keys, publish manifests, or trusted state.

### Lifecycle

`mkDevShell` starts the service container by default because the project Nix cache is default-on. `nixCache = false` opts out of cache service/integration but still starts the service container when the Beads-owned workspace database exists. `wrix run` and `wrix spawn` call `wrix service start` as an idempotent ensure/health-check before launching the agent container; if the service is already healthy, this is a no-op.

The service container has a caller-independent lifecycle invariant: stopping the shell, editor, or service that evaluated shellHook does not tear down or SIGKILL the workspace service container. `wrix service stop` removes only the selected workspace's service container. Cache-only service startup is suppressed for temp-directory scratch workspaces, so `/tmp` test or integration directories do not create persistent service containers. Loom bead clones and the independent integration clone under `.loom/` share the outer repository's service identity instead of starting persistent `*-service` containers named after the internal path.

### Service image source

The `<repo>-service` container image is a wrix-managed Nix-built image.
`image-builder.md` owns its source metadata and ownership labels, while
`sandbox.md` owns installation and retention through the shared runtime image
path. This spec owns only the service lifecycle's use of that image path.

### Service operations

`cli.md` owns public command placement, root help behavior, and the packaged
binary surface. This spec owns the operations delegated to its service command
group:

- lifecycle and diagnostics: `start`, `stop`, `status`, `logs`, `endpoints`;
- Dolt endpoint and maintenance operations: `status`, `socket`, `port`, `host`,
  `attach`, `gc`, `wait`, `sandbox-endpoint`;
- project-cache operations: `status`, `publish`, `warm`, `prune`, `rotate-key`.

The delegated implementation is Rust-first. Service internals are proper Rust
crates and helper binaries:

- `wrix-service` — service-container lifecycle, endpoint metadata, port leasing, Dolt service management.
- `wrix-cache` — project-cache library plus helper binaries `wrix-cache-hook`, `wrix-cache-publish`, and `wrix-cache-serve`; the HTTP cache server lives here rather than in a separate crate.

Nix may install immutable helper binaries for privileged hook/static-server entry points.

### Dolt service

The Dolt service exposes the same logical endpoint surfaces that beads uses today:

- Linux host and sandbox clients use the workspace Dolt Unix socket when available.
- Darwin clients use TCP because VirtioFS does not reliably carry Unix-socket operations.

When Darwin or another fallback needs Dolt TCP, the service binds only a host-loopback port and publishes the exact endpoint through `services.json` / `wrix service endpoints`. Linux does not publish a Dolt TCP port by default.

TCP service startup provisions the workspace's default SQL login for forwarded
connections, including when an existing privileges database contains only a
localhost login. Startup preserves database contents, unrelated users/grants,
and existing passwords. Services created without this initialization are
recreated on the next `wrix service start`, using the same mounted database.
TCP readiness requires a successful authenticated SQL query through the
published endpoint; accepting a TCP connection alone is insufficient.

### Project Nix cache transport

The project cache uses the Nix binary-cache protocol with different host and sandbox transports:

- **Host reads** use `file://<cache-root>` as an extra substituter plus the generated project cache public key. Host Nix does not need a cache HTTP port.
- **Host writes** copy selected store paths into `<cache-root>` with the host-only signing key, update matching GC markers, and mark the cache dirty.
- **Sandbox reads** use a launcher-resolved HTTP URL for the service container's static read-only cache server. The launcher injects that URL, the public key, and `builders-use-substitutes = true` into container `NIX_CONFIG`.
- **Sandbox writes** are out of scope for v1: sandboxes receive no cache signing key and no write-capable endpoint.

The service container serves `<cache-root>` with a Rust static HTTP helper mounted read-only. The server accepts GET/HEAD only, disables directory listing, rejects path traversal, and serves only Nix binary-cache paths (`nix-cache-info`, `*.narinfo`, `nar/...`, optional `log/...`). Wrix does not rely on container DNS. The service publishes a host-loopback port (`127.0.0.1:<cache-port> -> service-container:8080`), persists the selected endpoint in `services.json`, and launchers inject the resolved sandbox-visible URL. Preferred host ports are deterministic from the workspace hash: cache HTTP uses `21000–22999`, and Darwin/fallback Dolt TCP uses `23000–24999`. If a preferred port is busy, wrix probes within the matching range, persists the chosen port, and reuses it while available. Service startup fails if the runtime cannot bind required service ports to loopback only.

HTTP clients are isolated from each other and from the accept loop. The helper admits at most 32 concurrent requests, bounds headers to 8 KiB and 32 fields within five seconds, and bounds writes to 30 seconds of inactivity and five minutes total. GET streams files with bounded memory; HEAD reads only metadata. When cache and Dolt share the service container, either process exiting terminates the other and fails the container rather than hiding a failed service.

HTTP is the only container-facing cache transport. Unix-socket cache substituters, host Nix daemon sockets, shared mutable `/nix/store` volumes, Harmonia, nix-serve, and other host-store-serving caches are excluded.

### Project scope and root sets

A path is publishable only when it is reachable from a configured root of the current workspace flake. Wrix distinguishes **publishable roots** from **warm roots**:

- Default publishable roots: current-system `packages`, current-system `checks`, and the selected `devShell`. If a user runs `nix flake check`, check outputs may enter the project cache.
- Default warm roots: current-system `packages` and the selected `devShell`. Checks are built by proactive warm only when explicitly requested (for example `wrix service cache warm --checks` or matching config).

`nixCache` attrset form lets consumers add/remove roots with symmetric `includeRoots` / `excludeRoots`; excludes win. Root entries are flake installables or explicit flake attr paths, not arbitrary `/nix/store` paths.

Before copying, publishing computes the eligible root closure and subtracts paths already available from configured upstream substituters. It then copies the remaining paths with `--no-recursive`, so the project cache contains project-specific misses while dependencies still available from upstream caches stay upstream-owned.

### Host Nix integration and publish hook

By default, `mkDevShell` configures host Nix to pull from the project cache and to publish successful project-scoped builds back to it. This requires a trusted Nix daemon setup because `post-build-hook` and trusted substituter/key settings are privileged Nix options. Host setup is one-time per machine/user/group (for example adding the user or a wrix group to Nix `trusted-users` and restarting the daemon), not per repository. If host Nix ignores the required substituter, trusted public key, `builders-use-substitutes`, or post-build hook, devshell entry fails loud with remediation; `nixCache = false` is the explicit opt-out.

The hook is a project-specific immutable wrapper in the Nix store. It bakes in the workspace hash, workspace owner uid/gid, state root, cache root, manifest path, and immutable publisher-helper path. When the Nix daemon invokes it as root (Linux and normal multi-user Darwin), the wrapper performs only static validation and portable privilege drop to the workspace owner, then execs the user-owned publisher helper. The wrapper and publisher never evaluate Nix roots as root, source shell code, execute repo files, or read authoritative publish data from the workspace.

Automatic post-build publishing is scoped by `<state-root>/publish-roots.json`. The publisher acts only when `DRV_PATH` matches a configured publish-root derivation for this workspace; when it matches, the publisher copies that root's realized closure. It does not publish every derivation built under a project devshell. `wrix service cache publish` and `wrix service cache warm` refresh the manifest and repair stale state. Shell entry does not eagerly evaluate all roots; if the manifest is missing or stale, it prints a short reminder unless `WRIX_NIX_CACHE_REMINDER=0` is set.

Nix supports one `post-build-hook` per invocation. If a non-wrix hook is already configured, wrix fails loud by default rather than replacing or chaining arbitrary root-run hooks.

### Cache commands, locking, and retention

`wrix service cache publish` is realized-only: it evaluates configured publish roots, refreshes `publish-roots.json`, publishes roots already realized in the host store, drains matching pending candidates, updates GC markers, prunes unreachable cache entries, and reports unrealized roots without building them.

`wrix service cache warm` is explicit/manual only: wrix does not run warm automatically in devshell entry or sandbox launch. Warm builds configured warm roots, publishes them, updates GC markers, and prunes. `--checks` (or config) includes checks in proactive warm. Explicit cache commands fail nonzero on real errors.

All publish/prune/rotate operations take `<state-root>/cache.lock`. The post-build publisher waits for the lock and prints a waiting message on contention. If the wait times out, it records the hook's `DRV_PATH`/`OUT_PATHS` as a durable JSON file under `pending/`, prints a warning, and exits 0 so the completed Nix build is not failed solely by lock contention. Automatic publish failures after lock acquisition record status and warn; explicit cache commands remain strict. Pending records have a default retention of seven days and are reported by `wrix service cache status`.

GC markers under `gcroots/` govern retention. Updating a root replaces that root's marker with current outputs. Successful explicit publish/warm reconciles marker ownership against the full configured publishable-root set before pruning: removed or excluded roots lose their markers, while configured but unrealized roots retain theirs. Warm refreshes the full publish manifest, not just its build subset, and publishes only roots in the publishable set. Automatic publishing does not reconcile unrelated markers. Full prune runs after explicit `publish` / `warm` / `prune`, and on service startup only when the cache is dirty and the last prune is older than the configured interval (default 24h). The hook does not run a full prune in the hot path.

`wrix service cache rotate-key` takes the cache lock, invalidates/wipes the local project cache, generates a new keypair, updates metadata, and leaves repopulation to post-build publishing or explicit warm. V1 does not support old+new keyrings or in-place re-signing.

`wrix service cache status` reports cache size, pending count/age, last publish/prune status, dirty state, current endpoints, and warnings. The default soft size warning is 50 GiB; it is not a hard cap, and prune does not delete reachable current-root entries solely to satisfy the warning threshold.

### Remote builders

Direct remote-builder access to the local project cache is out of scope for v1. Remote builders may build normally; after outputs are available in the host store, host-side publishing can publish eligible outputs into the local project cache for later sandbox substitution. Wrix does not bind the project cache to LAN/VPN addresses or configure remote builder trust for the local cache by default.

## Success Criteria

- Concurrent service starts for the same workspace are serialized and create
  at most one replacement service container
  [test](../crates/wrix-cli/tests/service_lifecycle.rs::concurrent_service_starts_share_one_lifecycle_owner)

- `wrix service dolt sandbox-endpoint` returns the running Apple service VM's
  non-loopback TCP host and internal SQL port, or fails if no guest-visible
  address exists; host publication remains loopback-only
  [test](../crates/wrix-cli/tests/service_lifecycle.rs::apple_sandbox_endpoint_uses_service_vm_instead_of_host_loopback)

- A workspace that starts services gets a container named `<repo>-service`; the same service identity path yields the same container name, preferred service ports, state roots, and cache root, while two different checkout paths do not collide
  [test](../crates/wrix-service/tests/lifecycle.rs::workspace_identity_is_stable_and_collision_resistant)
- `mkDevShell` starts the service container by default for the project cache, `nixCache = false` suppresses cache-only startup, and any service container survives the process that evaluated the shell hook
  [system](test-ci:test-services-devshell-start-independent)
- `wrix service start` delegates service-image installation to the shared
  runtime image installer using the source metadata defined by
  `image-builder.md`, with no service-specific install path
  [system](verify:services.start-loads-image-source)
- Cache-only service startup is suppressed for temp-directory scratch workspaces, so tests and integration runs do not accumulate `tmp.*-service` containers
  [system](verify:services.temp-cache-only)
- Loom bead clone paths under `.loom/beads/<id>` use the outer repository service identity, so launches do not accumulate bead-named `*-service` containers
  [test](../crates/wrix-service/tests/lifecycle.rs::loom_bead_workspace_uses_repo_service_identity)
- Loom's independent clone under `.loom/integration` uses the outer repository service identity, so devshell entry does not accumulate integration-named `*-service` containers
  [test](../crates/wrix-service/tests/lifecycle.rs::loom_integration_workspace_uses_repo_service_identity)
- Rust packaging exposes the explicit `wrix-cache-hook`, `wrix-cache-publish`, and `wrix-cache-serve` helper binaries from `wrix-cache`
  [check](verify:services.rust-helper-binaries)
- Linux beads clients reach Dolt through the workspace Unix socket, while Darwin beads clients receive the service container's TCP host/port endpoint
  [system](verify:services.dolt-platform-transport)
- Fresh TCP Dolt services accept the default SQL login for forwarded connections
  [test](lifecycle::dolt::test::fresh_tcp_service_provisions_a_forwarded_root_login)
- TCP startup repairs persisted localhost-only grants idempotently without losing database contents or unrelated users/grants
  [test](lifecycle::dolt::test::tcp_bootstrap_repairs_persisted_localhost_grants_without_losing_state)
- TCP startup does not reset an existing SQL login password
  [test](lifecycle::dolt::test::tcp_bootstrap_does_not_reset_an_existing_root_password)
- Existing TCP services without authentication initialization are recreated once against the same database on service startup
  [test](../crates/wrix-cli/tests/service_lifecycle.rs::apple_start_recreates_legacy_tcp_services_for_authentication_bootstrap)
- TCP Dolt readiness rejects a reachable server that refuses SQL authentication
  [test](lifecycle::dolt::test::sql_probe_rejects_a_reachable_server_with_invalid_credentials)
- TCP Dolt readiness fails within a bounded startup budget when a listener accepts connections but never speaks SQL
  [test](../crates/wrix-cli/tests/service_lifecycle.rs::tcp_wait_rejects_a_listener_without_sql_authentication)
- Default `mkDevShell` cache enablement creates Linux XDG state/cache roots or Darwin Library state/cache roots, plus GC-root directory, signing key, public key, publish-root manifest, pending directory, lock file, status file, and endpoint metadata outside `/workspace`; `nixCache = false` does not create cache state solely for cache use
  [system](verify:services.cache-state-layout)
- Host devshell Nix uses `file://<cache-root>` as the project cache substituter, trusts the generated public key, enables `builders-use-substitutes`, installs a project-specific immutable post-build hook, and fails loudly when the host Nix daemon ignores any required setting
  [system](verify:services.host-nix-config)
- `wrix run` and `wrix spawn` inject container `NIX_CONFIG` that points at the project cache HTTP endpoint, trusts only the generated public key for that cache, and enables `builders-use-substitutes`
  [system](verify:services.container-pull-config)
- The service cache HTTP endpoint is a Rust static read-only server for `<cache-root>`, uses an explicit persisted loopback host port in the `21000–22999` range, serves only Nix binary-cache paths, and does not require container DNS for sandbox substitution
  [system](verify:services.cache-http-endpoint)
- An idle cache client does not block independent requests
  [test](../crates/wrix-cache/tests/helper_server.rs::idle_clients_do_not_block_other_requests)
- Disconnected or reset cache clients cannot terminate the HTTP helper
  [test](../crates/wrix-cache/tests/helper_server.rs::disconnected_clients_do_not_terminate_service)
- Cache request headers are bounded and parsed completely before serving
  [test](../crates/wrix-cache/tests/helper_server.rs::request_headers_are_bounded_and_complete)
- Trickle-fed request headers cannot extend the request deadline
  [test](../crates/wrix-cache/tests/helper_server.rs::partial_request_deadline_is_not_extended_by_trickling)
- Stalled response writes obey a bounded deadline
  [test](helper::server::test::stalled_body_writes_obey_the_response_deadline)
- HEAD uses metadata only and large GET responses stream within bounded memory
  [test](../crates/wrix-cache/tests/helper_server.rs::head_reads_only_metadata_and_get_streams_large_files)
- Legacy combined cache/Dolt containers are recreated once with child supervision on service start
  [test](../crates/wrix-cli/tests/service_lifecycle.rs::legacy_combined_services_are_recreated_with_child_supervision)
- A combined cache/Dolt service fails and stops its sibling when either child exits
  [test](lifecycle::supervisor::test::either_service_exit_stops_its_sibling_and_fails_the_container)
- Sandboxes receive no cache signing key, no durable state root mount, no host `/nix/store` mount, and no host Nix daemon socket as part of project-cache integration
  [system](verify:services.sandbox-cache-boundary)
- With `WRIX_NETWORK=limit`, sandbox Nix can reach exactly the project cache endpoint while unrelated host-local services remain outside the generated allowlist
  [system](test-ci:test-services-limit-mode-cache-endpoint)
- The post-build hook drops privileges before publishing, never executes workspace files, and publishes only when `DRV_PATH` matches a configured publish-root derivation in `<state-root>/publish-roots.json`
  [test](../crates/wrix-cache/tests/hook.rs::post_build_hook_scopes_publish_to_manifest_roots)
- `wrix service cache publish` refreshes the publish manifest, publishes only already-realized configured roots, drains matching pending records, updates GC markers, and does not build missing roots
  [test](../crates/wrix-cache/tests/publisher.rs::publish_realized_roots_drains_pending_and_updates_gc_markers)
- `wrix service cache warm` builds default warm roots (packages plus selected devShell), excludes checks by default, includes checks with `--checks`, then publishes and prunes
  [test](../crates/wrix-cache/tests/publisher.rs::warm_roots_include_checks_only_when_requested)
- Publishing copies only paths reachable from configured current-workspace flake roots, subtracts paths already available from upstream substituters, and never publishes an arbitrary host store path outside that scoped closure
  [test](../crates/wrix-cache/tests/publisher.rs::publish_filters_to_current_workspace_closure)
- Publishing uses the flat Nix binary-cache layout with signed narinfos and `--no-recursive` copies of the filtered path set
  [test](../crates/wrix-cache/tests/publisher.rs::publish_uses_flat_signed_cache_with_nonrecursive_copies)
- Lock contention in the automatic publisher prints a waiting message; lock timeout writes a pending JSON record and warning, and a later explicit publish drains matching pending records
  [test](../crates/wrix-cache/tests/publisher.rs::lock_timeout_records_pending_and_explicit_publish_drains)
- Explicit publishing removes markers and unreachable payloads for removed, renamed, excluded, or empty root sets without removing unrelated pending records
  [test](../crates/wrix-cache/tests/publisher.rs::publishing_reconciles_removed_renamed_excluded_and_empty_root_sets)
- Marker reconciliation preserves temporarily unrealized current roots and payloads shared with retained roots
  [test](../crates/wrix-cache/tests/publisher.rs::reconciliation_preserves_unrealized_roots_and_shared_payloads)
- Warming a subset keeps the full publish manifest and retains configured check roots, while respecting publish exclusions
  [test](../crates/wrix-cache/tests/publisher.rs::warm_subsets_preserve_full_publish_manifest_and_check_retention)
- Automatic publishing does not remove unrelated root markers
  [test](../crates/wrix-cache/tests/publisher.rs::automatic_publish_does_not_reconcile_other_root_markers)
- Updating a root to a new output replaces that root's GC marker and prunes cache entries no longer reachable from any marker on explicit prune/publish/warm, so repeated publishes do not grow the cache indefinitely
  [test](../crates/wrix-cache/tests/publisher.rs::prune_keeps_only_paths_reachable_from_current_markers)
- `wrix service cache rotate-key` invalidates the local cache, generates a new keypair, updates metadata, and requires republishing rather than trusting old and new keys simultaneously
  [test](../crates/wrix-cache/tests/publisher.rs::rotate_key_invalidates_cache_and_replaces_trust_root)
- `wrix service cache status` reports cache size and warns above the default 50 GiB soft threshold without deleting reachable entries solely to satisfy that threshold
  [test](../crates/wrix-cache/tests/publisher.rs::status_warns_above_soft_size_without_pruning)
- The container-facing cache transport is HTTP only; wrix does not configure Unix-socket cache substituters, host Nix daemon sockets, shared mutable `/nix/store`, or host-store-serving tools for sandbox cache reads
  [check](verify:services.cache-transport-http-only)

## Requirements

### Functional

1. **Workspace identity** — services are keyed by the canonical repository root for paths inside a Git checkout, and by the canonical current path otherwise. Container names use `<repo>-service`; ports and state/cache roots derive from the identity path hash. Loom-managed paths under `.loom/` use the outer repository identity.
2. **Service operations** — the service command group provides lifecycle,
   diagnostics, Dolt endpoint maintenance, and project-cache operations.
   Separable internals are proper Rust crates and helper binaries.
3. **Lifecycle management** — `wrix service start` is idempotent and caller-independent; `stop`, `status`, `logs`, and endpoint queries operate on the selected workspace only. Cache-only starts in temp-directory scratch workspaces are no-ops rather than persistent service containers.
4. **Service image source** — `wrix service start` installs the selected service
   image through the shared runtime image installer. `image-builder.md` owns
   source metadata and labels; `sandbox.md` owns install dispatch and image
   retention.
5. **Dolt hosting** — when the Beads-owned workspace database exists, the service container runs its Dolt SQL server and publishes the endpoint in the shape `beads.md` expects.
6. **Cache enablement** — `mkDevShell` enables the standard project cache by default; `nixCache = false` disables it.
7. **Host cache pull** — enabled host devshells configure Nix to use `file://<cache-root>` plus the generated public key and `builders-use-substitutes = true`.
8. **Host cache push** — successful host builds of manifest-matching workspace roots publish the filtered path set into the project cache with the host-only signing key.
9. **Sandbox cache pull** — `wrix run` and `wrix spawn` pass the resolved project cache HTTP endpoint and public key into the container so in-container Nix substitutes from the cache before building.
10. **No sandbox cache push** — containers do not receive the signing key or write-capable cache endpoint. Container publishing is out of scope for v1.
11. **Project-scoped publishing** — publishing starts from configured current-workspace flake roots and subtracts upstream-substitutable paths before copying. The host store is never scanned as a source of publish candidates.
12. **Bounded retention** — root markers, explicit prune, dirty/stale startup prune, pending TTL, and key rotation keep cache state bounded and explainable.
13. **HTTP-only sandbox reads** — sandbox-facing cache reads use HTTP. Unix sockets, host Nix daemon sockets, shared mutable `/nix/store` volumes, and host-store-serving caches are excluded.

### Non-Functional

1. **Project isolation** — each service identity path has an independent service container, state root, cache root, signing key, port leases, and retention marker set.
2. **Host-store isolation** — sandboxes can substitute signed project-cache narinfos, but they cannot read the host `/nix/store`, talk to the host Nix daemon, or mount durable cache state.
3. **Local-first operation** — the project cache is local to the developer machine by default. Hosted binary caches are optional upstreams, not required for the wrix-managed cache.
4. **Bounded growth** — cache size is governed by current root markers, pending TTL, and pruning rather than session count or historical build count; 50 GiB is a default warning threshold, not a hard cap.
5. **Fail-loud trust configuration** — when host Nix cannot honor required cache settings because system trust is missing, wrix reports that fact instead of pretending host pull/push is active.
6. **No DNS dependency** — sandbox cache access is through a launcher-injected resolved endpoint, not a container-name DNS convention.

## Out of Scope

- A singleton per-host service container. Per-workspace containers remain the isolation model for this version.
- Host-store-backed sandbox cache designs beyond the project-cache boundary
  cross-referenced by `security.md`.
- Hosted cache management (Cachix, Attic, niks3, S3, or GHCR). They may be user-configured upstream substituters, but wrix's project cache is local.
- Direct remote-builder access to the local project cache. Remote-built outputs may be published after they are available in the host store.
- Sandboxed cache publishing or host-validated promotion. Sandboxes are read-only cache consumers in v1.
- Non-Nix build artefacts such as `target/`, cargo registry/git caches, sccache objects, Python virtualenvs, or uv caches. Those remain profile/tool-specific cache surfaces.
