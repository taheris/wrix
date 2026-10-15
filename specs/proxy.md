# Enforced Egress Proxy

Authenticated HTTP/CONNECT admission for restricted sandbox networking, cohosted
in the shared host-user service container.

## Problem Statement

Restricted launches need a mandatory, inspectable public-egress path without
placing network policy under agent control. The proxy admits requested hostnames
and ports while the sandbox firewall blocks direct bypass. It does not inspect
TLS or claim to prevent exfiltration through approved services.

## Architecture

Acceptance: [Proxy Acceptance](#proxy-acceptance).

`services.md` owns the single host-user container, workspace tenancy, lifecycle
and managed storage. The proxy is a component of that container, not a separate
VM per launch or workspace. `sandbox.md` owns `open`/`limit` selection and the
agent firewall; `profiles.md` owns allowlist membership; `security.md` owns the
cross-cutting threat and credential model.

Trusted launchers register immutable effective policies through host management
and issue scoped per-launch traffic credentials. One proxy serves multiple
workspaces without unioning their permissions. Identical policies may share
internal representation; credentials remain independently revocable. Source IP
is not client identity. Host management authority is separate from traffic
credentials and is neither mounted into sandboxes nor reachable through their
allowed data endpoints or proxy tunnels.

Proxy failure or missing authentication never enables direct egress. The shared
service boundary does not offer separate-container compromise or fault isolation
between proxy, Dolt, Celler and the shared sccache compiler. Managed data and
host-private publishing/management authority follow
[Services State Layout](services.md#state-layout); Celler's delegated
cache-signing authority follows [Cache Authority](cache.md#cache-authority).
Proxy isolation does not protect those cache keys from a sufficiently privileged
compromise of the shared service. Shared compiler jobs are deliberately inside
this service trust boundary, not independently isolated worker processes; proxy
enforcement assumes that infrastructure remains trusted.

## Destination Admission

Acceptance: [Proxy Acceptance](#proxy-acceptance).

The proxy accepts standard HTTP forwarding and CONNECT for authenticated
clients. It validates the requested authority and port against that client's
policy. Rules use the structured `host`/`ports` contract in
[Profiles Network Allowlist](profiles.md#network-allowlist), including explicit
ports for built-ins. A rule admits only its listed host/port pairs; there are no
implied ports, all-port defaults or hostname-only compatibility forms. Unknown
or revoked traffic credentials receive a proxy-authentication failure; denied
destinations receive a policy denial. Proxy authorization is not forwarded to
upstream services. HTTP streams and CONNECT tunnels use bounded
metadata/buffering rather than collecting entire payloads.

The proxy resolves permitted hostnames when handling requests and validates the
actual chosen destination before connecting. Unresolvable or prohibited answers
fail the request, not an unrelated agent launch merely because an unused
allowlist name is temporarily unavailable. Checked addresses are pinned for the
connection; unchecked re-resolution cannot open a rebinding gap. The protected
local/private/host/VPN/special-address baseline is owned by `sandbox.md` and
uses one shared definition, not a divergent proxy deny list. Proxy-side network
protection independently retains that baseline and only exact required DNS
exceptions. Sandbox public DNS is not an alternative unrestricted egress path.

CONNECT carries opaque bytes to an admitted endpoint. There is no TLS
termination, certificate substitution, trusted-CA injection or payload
inspection. The admission guarantee concerns the requested connection authority,
not encrypted SNI/HTTP identity, URL paths, repository identity or content.
Shared-IP virtual hosting and permitted-service exfiltration remain limitations.

## Grant Ownership

Acceptance: [Grant Ownership Acceptance](#grant-ownership-acceptance).

Each launcher owns one authenticated host-only control connection for its
launch. Registration binds the immutable policy and traffic credential to that
connection; traffic requests use separate data connections. Neither the agent
nor runtime subprocesses inherit the control connection or its management
authority.

When the proxy observes control disconnect, including orderly close, EOF or
transport failure, it revokes that connection's grant, rejects new requests and
closes associated HTTP streams and CONNECT tunnels. Other launch grants remain
unaffected. Normal launcher completion closes its ownership connection; explicit
workspace/host stop retains the scoped revocation contract in `services.md`.
Proxy/service restart does not restore old grants, and reconnect does not revive
a revoked credential: authorization requires fresh registration and a fresh
traffic credential.

Ownership requires no periodic application heartbeat, lease renewal or PID
polling. An idle or hung launcher with a live connection remains authorized.
Half-open connections can persist until transport failure is detected; no fixed
orphan-access expiry deadline is claimed. Local launcher death ordinarily closes
its socket, but revocation is triggered by observed disconnect, not an
assumption that every failure produces immediate notification.

Grant revocation is network authorization, not agent supervision or execution
completion. It neither proves the agent stopped nor fills in missing execution
metadata; `security.md` remains authoritative for those evidence limits.

## Launch Integration

Acceptance: [Proxy Acceptance](#proxy-acceptance).

`limit` requires ready proxy infrastructure and a launch-scoped grant before
agent work. The launcher supplies runtime-only proxy routing/authentication to
supported agent and developer tools. Git SSH uses a CONNECT-compatible tunnel
while preserving the existing deploy-key and pinned-host-key rules; it receives
no unfiltered direct-internet exception. Exact workspace cache/Dolt endpoints
remain direct Wrix exceptions, not permission to reach arbitrary local services.
Restricted Rust must use effective private tool caches under
[Shared Compiler Selection](profiles.md#shared-rust-compiler-service); otherwise
launch fails before preparation rather than delegating code outside the worker
firewall. No restricted launch receives a shared compiler route, grant or
firewall exception, including through a cache/Dolt endpoint or alternate role.

The agent cannot widen its policy by changing proxy environment variables, using
raw sockets, or selecting another workspace or management interface. Read-only
preflight reports policy and preparation needs without issuing grants or
starting the shared service; traffic credential values never appear in the
report. Grant lifetime and failure behavior follow
[Grant Ownership](#grant-ownership); execution completion remains independent.

## Success Criteria

### Proxy Acceptance

- Wrix-owned network rules use structured `host`/`ports` objects; bare strings,
  unknown rule fields and missing, empty or invalid ports fail before service
  preparation or container startup.
  [test?](../crates/wrix-cli/tests/proxy_policy.rs::missing_empty_or_invalid_ports_fail_before_preparation)
- Authenticated HTTP and CONNECT requests are admitted only for explicitly
  listed host/port pairs under the selected immutable launch policy, without
  implied ports, unioning other workspace permissions or trusting source IP.
  [system?](verify:proxy.policy-authentication)
- Missing, revoked or incorrect traffic credentials cannot access another
  policy, mutate policy registration or reach host management through direct
  requests or CONNECT. [system?](verify:proxy.management-separation)
- Resolution/connect handling rejects unresolvable or protected destinations,
  pins checked addresses, and rejects DNS-rebinding and alternate-DNS bypass.
  [system?](verify:proxy.destination-safety)
- HTTP forwarding strips proxy authorization and streams large payloads with
  bounded request metadata and buffering; CONNECT preserves opaque TLS bytes
  without interception or certificate substitution.
  [system?](verify:proxy.forwarding-contract)
- Live rootless Podman restricted launches permit authorized proxy requests but
  deny direct, raw-socket, UDP, public-DNS and shared-compiler delegation
  bypass, including on proxy failure and while an eligible peer uses a live
  compiler. [system?](verify:proxy.podman-enforcement)
- Live krun restricted launches satisfy the same mandatory-proxy controls with
  their own KVM/runtime prerequisite reporting, including compiler-route denial.
  [system?](verify:proxy.krun-enforcement)
- Live Apple Container restricted launches satisfy the same mandatory-proxy
  controls with their own host prerequisite reporting, including compiler-route
  denial. [system?](verify:proxy.apple-enforcement)
- Packaged agent/provider HTTP, Git SSH, Nix, Cargo and uv workflows route
  through the selected proxy while scoped workspace cache/Dolt access remains
  available; credentials and pinned Git host verification are preserved.
  [system?](test-ci:test-proxy-tooling)
- Preflight issues no traffic grant, normal completion revokes only its launch
  grant, and workspace/host stops revoke only their explicitly selected scope.
  [test?](../crates/wrix-cli/tests/proxy_grants.rs::traffic_grants_follow_launch_and_stop_scope)
- Documentation distinguishes connection admission from encrypted application
  identity/content enforcement, discloses the cohosted compiler/service boundary
  and trusted-service assumption, and forbids restricted compiler delegation.
  [judge?](../tests/judges/proxy-boundary.md)

### Grant Ownership Acceptance

- Registration binds each immutable policy/credential to its launch control
  connection; observed disconnect revokes only that connection's grant.
  [test?](../crates/wrix-service/tests/proxy_grants.rs::control_disconnect_revokes_only_its_launch_grant)
- Live control disconnect rejects new traffic and closes existing HTTP streams
  and CONNECT tunnels, while independently owned grants remain usable.
  [system?](verify:proxy.connection-owned-revocation)
- Runtime/agent child processes do not inherit control descriptors or management
  authority; a surviving child cannot keep a killed launcher's grant alive.
  [test?](../crates/wrix-cli/tests/proxy_grants.rs::runtime_children_do_not_inherit_control_connection)
- Reconnect and proxy/service restart cannot revive revoked credentials; fresh
  registration uses a distinct traffic credential.
  [test?](../crates/wrix-service/tests/proxy_grants.rs::old_credentials_do_not_revive_after_reconnect_or_restart)
- Idle connection ownership remains valid without periodic application renewal
  messages or a finite lease deadline.
  [test?](../crates/wrix-service/tests/proxy_grants.rs::idle_ownership_requires_no_application_renewal)
- Documentation discloses hung/half-open connection limits and does not equate
  grant revocation with agent termination or recorded execution completion.
  [judge?](../tests/judges/proxy-grant-ownership.md)

## Out of Scope

- Periodic application heartbeat/lease renewal, PID polling, and guaranteed
  time-bounded revocation for every host/network failure.

- TLS inspection/termination, certificate substitution or CA installation.
- Guarantees against domain fronting, shared-IP encrypted identity changes or
  exfiltration through approved services.
- General UDP/SOCKS forwarding, arbitrary direct-internet bypass, or a LAN-open
  mode; exact Wrix service exceptions are not general forwarding permissions.
- A separate proxy VM per workspace/launch, a machine-wide multi-user service,
  or a unioned host-wide allowlist.
- Runtime denial telemetry collectors, payload/transcript logging, permissive
  learning mode or automatic policy widening.
- Provider credential brokering, host-side agent supervision, automatic
  execution recovery, or an authoritative background observer of agent
  completion.
