# Services verifiers

## Verifier boundaries

`specs/services.md` binds Rust-owned contracts to native tests and assembled
transport contracts to `test-ci` apps. Dry-run ENV/MOUNT output and a simulated
Apple inventory are command-planning evidence, not container exposure or guest
SQL evidence.

| Criterion                   | Production seam                                                                      | Existing proof and bounded correction                                                                                                                                                                                                                                                                                                                                                                                                                                |
| --------------------------- | ------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Shared image installer      | `wrix-service::lifecycle::Runtime::ensure_image` delegates to `wrix-sandbox::image`  | Native image-install tests already cover source dispatch. `service_cache::service_start_uses_shared_image_digest_preflight_without_pruning` now runs the service CLI for both runtimes, on hits and misses, and asserts copy/load dispatch without pruning or retention writes. Live cache/beads apps also install the packaged service image.                                                                                                                       |
| Temp cache-only suppression | `Plan::from_parsed`, `start`                                                         | `wrix-service` already tests the suppressed plan. `service_cache::temp_cache_only_start_never_creates_a_service_container` additionally proves the CLI never creates a container, without disabling unrelated runtime status queries.                                                                                                                                                                                                                                |
| Cache state layout          | `Paths::for_workspace`, `Plan::ensure_layout`                                        | Native CLI coverage asserts exact current-platform XDG/Library roots, every cache artefact, real Nix key generation, and opt-out without cache state. `test-services-devshell-start-independent` and the retained shell-owned host-Nix tests cover default shell enablement and opt-out wiring. Darwin roots are tested on Darwin, not inferred from Linux simulations.                                                                                              |
| Container pull config       | `ServicesState::load_project_cache`, `Plan::env_pairs`                               | Existing `service_lifecycle::cache_guest_endpoint_and_key_reach_both_launch_commands` asserts actual subprocess argv for both launches and nested Loom paths. `test-services-limit-mode-cache-endpoint` additionally checks exact NIX_CONFIG against the host-generated public key inside both real launch modes.                                                                                                                                                    |
| Sandbox cache boundary      | Launcher mount construction                                                          | That existing native launch test rejects state/cache/store/daemon/secret mounts. The live cache app checks a host-only store sentinel and cache/state/key/daemon absence inside the guest; the Linux VM also checks mountinfo and the service's read-only cache mount.                                                                                                                                                                                               |
| Cache HTTP endpoint         | Service port leasing, static helper, launcher numeric endpoint resolution            | `wrix-cache::helper_server::static_server_enforces_binary_cache_path_policy` already proves GET/HEAD-only and path policy through a real helper. Native Apple endpoint/address/firewall tests cover guest resolution separately from persisted host publication. The live cache app proves loopback/ranged publication, numeric guest HTTP access to cache-root payloads (Apple VM:8080 on Darwin), stable endpoints, and isolation from an unrelated host listener. |
| Dolt platform transport     | Default `DoltTransport`, `ServicesState::load_dolt`, Apple guest endpoint resolution | Native endpoint payload/default-transport/Apple guest-address and SQL authentication tests remain intact. `test-services-dolt-platform-transport` runs the existing beads VM on Linux (real shared Unix socket and guest SQL/sync); on Darwin it runs the live Apple service and authenticates guest SQL through the non-loopback VM address/internal port, also checking host-loopback publication. Host TCP tests are not Darwin guest proof.                      |

The seven audited annotations now resolve to native tests or `test-ci` apps;
their obsolete generic services IDs are absent from `.#verify --list`. The cache
and Linux beads apps use the declared direct command-runner fixture, whose
argv/stdio contract is tested by `test_command_runner_package_contract`;
guest-written success markers are required so an agent that exits without
executing the probe cannot pass. Native runtime fixtures have conformance tests
and mock only external container/image commands; key generation and Rust service
and launcher logic are real. The legacy lifecycle image-source smoke may still
be run explicitly for its Nix-built source boundary; it is not the authoritative
native image-delegation binding.

No publisher, key, server-policy, identity, or service startup contract is
changed. Platform/prerequisite skips remain gaps in live execution evidence, not
passing transport results.

## VM execution

The Linux `test-services-devshell-start-independent` and
`test-services-limit-mode-cache-endpoint` CI apps build the fixtures' packaged
NixOS test drivers, then execute their real guest assertions in an isolated
temporary directory. Runner preparation does not realize VM test results. The
ordinary `systemTests` derivations remain available for direct Nix builds.

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
