# Services verifiers

## Verifier boundaries

`specs/services.md` binds Rust-owned contracts to native tests and assembled
transport contracts to `test-ci` apps. Dry-run ENV/MOUNT output and a simulated
Apple inventory are command-planning evidence, not container exposure or guest
SQL evidence.

| Criterion                   | Production seam                                                                      | Existing proof and bounded correction                                                                                                                                                                                                                                                                                                                                                                                                                                |
| --------------------------- | ------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Shared image installer      | `wrix-service::lifecycle::Runtime::ensure_image` delegates to `wrix-sandbox::image`  | Native image-install tests already cover source dispatch. `service_cache::service_start_uses_shared_image_digest_preflight_and_retention` now runs the service CLI for both runtimes, on hits and misses, and asserts copy/load dispatch and shared MRU. Live cache/beads apps also install the packaged service image.                                                                                                                                              |
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
NixOS test drivers, then execute their unchanged guest assertions in an isolated
temporary directory. Runner preparation does not realize VM test results. The
ordinary `systemTests` derivations remain available for direct Nix builds.

The `virtiofsd-capabilities` prerequisite checks the VM executor, not a Nix
daemon or remote builder. The pinned virtiofsd, with the NixOS driver's
`--sandbox=none` and default inode handles, installs CHOWN, DAC_OVERRIDE,
DAC_READ_SEARCH, FOWNER, FSETID, SETGID, SETUID, MKNOD and SETFCAP when run as
root. A capability absent from the Linux bounding, permitted and inheritable
sets cannot be acquired by the executor. A dropped bounding bit alone is not
sufficient evidence when the capability is already permitted or inheritable.
Unprivileged virtiofsd does not install this root capability set. Malformed
capability data is a preflight failure, not an exemption.

Both fixtures permit TCG (`requiredFeatures.kvm = false`); missing KVM and
container nesting are not reasons to skip them. Successful preflight does not
guarantee a passing VM: driver preparation, invocation and guest failures,
including exit 77, remain failures. No log text is used to classify a skip.

A genuine prerequisite gap produces exit 77 and structured skipped metadata.
Loom's declared test-ci capability policy permits that gap for worker-stage
acceptance only. It does not provide live coverage or a push marker; the
integration-branch host-test stage still owns live execution.

`checks.<system>.system-test-prerequisites` exercises the packaged wrapper with
external Nix/driver fixtures and actual kernel preflight.
`verify:prek.ci-batching` also exercises runner preparation, mixed-batch failure
dominance and Loom's worker/host consumption. Fixture passes are conformance
evidence, not passing services VM results.
