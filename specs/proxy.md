# Enforced Egress Proxy

Authenticated HTTP/CONNECT admission for restricted sandbox networking, managed
within the host-user service deployment.

## Problem Statement

Restricted launches need a mandatory, inspectable public-egress path without
placing network policy under agent control. The proxy admits requested hostnames
and ports while the sandbox firewall blocks direct bypass. It does not inspect
TLS or claim to prevent exfiltration through approved services.

## Architecture

Acceptance: [Proxy Acceptance](#proxy-acceptance).

`services.md` owns host-user deployment identity, workspace tenancy, managed
storage and replaceable container generations. The proxy is shared
infrastructure, not a separate VM per launch or workspace. `sandbox.md` owns
`open`/`limit` selection and the agent firewall; `profiles.md` owns allowlist
membership; `security.md` owns the cross-cutting threat and credential model.

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

## Proxy Engine

Acceptance: [Proxy Acceptance](#proxy-acceptance) and
[Grant Ownership Acceptance](#grant-ownership-acceptance).

The replaceable Rust gateway embeds Rama as its HTTP forwarding and opaque
CONNECT engine. The host controller separately uses Rama for fixed-role byte
routing under [Host Control](services.md#host-control-service), not a second
general destination proxy or guest-accessible management plane. Wrix owns launch
authentication, immutable host/port policy, the shared protected address
baseline and connection-owned grant lifecycle. Rama supplies protocol handling
and transport primitives, not an independent source of policy or a second
management interface. The general Rama CLI, demo routes and client-selected
upstream proxy routing are not exposed.

Both HTTP and CONNECT use Wrix-authorized destination selection and checked
numeric-address dialing. Authentication and policy admission apply to every
request, including requests that reuse a connection. Connection reuse cannot
bypass admission, transfer a grant's authority to a peer or prevent independent
revocation. Cancellation covers pending resolution/connection work, streaming
HTTP request/response bodies and CONNECT relays, including work still active
after response headers have been returned. Revocation racing with admission
cannot establish new authorized traffic for the revoked grant.

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

Each launcher owns one authoritative authenticated host-only control connection
for its launch. The host controller coordinates registration/handoff without
persisting live grants or taking over launcher lifetime; gateway owner loss
retains the revocation contract below. Registration binds the immutable policy
and traffic credential to that live ownership; traffic requests use separate
data connections. Neither the agent nor runtime subprocesses inherit the control
connection or its management authority.

When the proxy observes loss of the authoritative control connection, including
orderly close, EOF or transport failure, it revokes that grant, rejects new
requests and closes associated HTTP streams and CONNECT tunnels across every
participating generation. Other launch grants remain unaffected. Normal launcher
completion closes its ownership connection; explicit workspace/host stop retains
the scoped revocation contract in `services.md`. Proxy/service restart does not
restore old grants, and reconnect does not revive a revoked credential:
authorization requires fresh registration and a fresh traffic credential. Only
the authenticated transfer of still-live ownership in
[Planned Gateway Handoff](#planned-gateway-handoff) preserves an active grant;
it cannot restore a disconnected or revoked one.

Ownership requires no periodic application heartbeat, lease renewal or PID
polling. An idle or hung launcher with a live connection remains authorized.
Half-open connections can persist until transport failure is detected; no fixed
orphan-access expiry deadline is claimed. Local launcher death ordinarily closes
its socket, but revocation is triggered by observed disconnect, not an
assumption that every failure produces immediate notification.

Grant revocation is network authorization, not agent supervision or execution
completion. It neither proves the agent stopped nor fills in missing execution
metadata; `security.md` remains authoritative for those evidence limits.

## Planned Gateway Handoff

Acceptance: [Planned Handoff Acceptance](#planned-handoff-acceptance) and
[Grant Ownership Acceptance](#grant-ownership-acceptance).

Existing sandboxes keep their Wrix proxy endpoint, traffic credential,
environment and firewall exception during a planned generation update. Trusted
routing sends new connections to Green and retains existing connections on Blue
under [Generation Handoff](services.md#generation-handoff). DNS changes, agent
cooperation or a sandbox restart are not the handoff mechanism; existing
TCP/CONNECT connections are not migrated between servers.

The waiting launcher and trusted control plane authenticate transfer of a
still-active grant before Green-dependent admission. Preserve the exact
immutable policy and data credential so existing tools need no credential
rewrite. A traffic credential alone, a saved grant, an old generation or an
agent cannot authorize transfer. Changed policy requires ordinary new admission;
never union Blue/Green or peer permissions to make handoff succeed.

There is one authoritative live owner throughout the transition. A candidate
control channel is not an independent second owner. The ownership transition
must distinguish a confirmed transfer from control loss: closing the former
connection after confirmed transfer does not revoke the now-transferred grant,
but losing authoritative ownership before confirmation revokes it and
invalidates the candidate rather than resurrecting it. Losing the new owner
after transfer cancels that launch's work in both generations, including old
streams, pending connects and body/background work. Stale transfer messages
cannot reverse revocation or overwrite current ownership.

Green must be ready to enforce this ownership and the unchanged policy before
routing relies on it. Failed preparation/transfer leaves no newly authorized
access; report any partial effects honestly. Ordinary crash/restart/reconnect
still requires fresh registration and a distinct credential, not restoration
from generation metadata. This planned live transfer introduces no lease,
periodic application heartbeat, detached agent supervisor or persistent grant
recovery service.

A launch can outlive Blue once ownership has transferred and its old data
connections close. Normal updates do not break an open SSH/CONNECT session or
force retirement on a timer; the last selected gateway remains selected even
when stopped. Services owns retirement and native Podman/Apple routing proof,
not a guarantee that every update of the stable routing layer is transparent.

## Launch Integration

Acceptance: [Proxy Acceptance](#proxy-acceptance).

`limit` requires ready proxy infrastructure and a launch-scoped grant before
agent work. The launcher supplies runtime-only proxy routing/authentication to
supported agent and developer tools. Git SSH uses a CONNECT-compatible tunnel
while preserving the existing deploy-key and pinned-host-key rules; it receives
no unfiltered direct-internet exception. Exact shared-role cache/Dolt endpoints
with workspace-scoped authorization remain direct Wrix exceptions, not
permission to reach arbitrary local services. Restricted Rust must use effective
private tool caches under
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
SQL maintenance admission belongs to
[Dolt Backend Handoff](services.md#dolt-backend-handoff), not the egress-grant
revocation mechanism.

## Success Criteria

### Proxy Acceptance

- The packaged Rust proxy uses embedded Rama for HTTP forwarding and opaque
  CONNECT under Wrix-owned authentication, destination policy and control-plane
  wiring; no general Rama CLI, demo API or client-selected upstream routing
  becomes a sandbox-facing interface. [system?](verify:proxy.rama-integration)
- Wrix-owned network rules use structured `host`/`ports` objects; bare strings,
  unknown rule fields and missing, empty or invalid ports fail before service
  preparation or container startup.
  [test](../crates/wrix-cli/tests/proxy_policy.rs::missing_empty_or_invalid_ports_fail_before_preparation)
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

- Registration binds each immutable policy/credential to authoritative live
  ownership; observed loss revokes that launch across generations without
  affecting independently owned grants.
  [test?](../crates/wrix-service/tests/proxy_handoff.rs::authoritative_disconnect_revokes_across_generations_not_peers)
- Live control disconnect rejects new traffic, cancels pending resolution and
  connection work, and closes HTTP upload/download streams and CONNECT tunnels,
  including after response headers and during admission races; independently
  owned grants remain usable.
  [system?](verify:proxy.connection-owned-revocation)
- HTTP keep-alive and pooled connections cannot bypass per-request admission,
  transfer authority between grants or couple their revocation; revoking one
  active grant leaves the other's authorized traffic usable.
  [system?](verify:proxy.grant-pooling-isolation)
- Runtime/agent child processes do not inherit control descriptors or management
  authority; a surviving child cannot keep a killed launcher's grant alive.
  [test?](../crates/wrix-cli/tests/proxy_grants.rs::runtime_children_do_not_inherit_control_connection)
- Ordinary reconnect and proxy/service restart cannot revive revoked
  credentials; fresh registration uses a distinct traffic credential. Planned
  transfer cannot revive already-lost ownership.
  [test?](../crates/wrix-service/tests/proxy_grants.rs::old_credentials_do_not_revive_after_reconnect_or_restart)
- Idle connection ownership remains valid without periodic application renewal
  messages or a finite lease deadline.
  [test?](../crates/wrix-service/tests/proxy_grants.rs::idle_ownership_requires_no_application_renewal)
- Documentation discloses hung/half-open connection limits and does not equate
  grant revocation with agent termination or recorded execution completion.
  [judge?](../tests/judges/proxy-grant-ownership.md)

### Planned Handoff Acceptance

- Authenticated live handoff preserves the same worker endpoint, credential and
  exact immutable policy without agent changes, management delivery or
  permission union; traffic credentials alone cannot authorize a transfer.
  [test?](../crates/wrix-service/tests/proxy_handoff.rs::live_handoff_preserves_policy_and_worker_configuration)
- Ownership commits to one live authority; expected former-channel close after
  confirmation is distinguished from loss before commit, which revokes the grant
  and invalidates pending transfer rather than reviving it.
  [test?](../crates/wrix-service/tests/proxy_handoff.rs::ownership_commit_distinguishes_transfer_from_disconnect)
- Current-owner loss after handoff cancels old/new-generation work and rejects
  stale transfer/admission messages while independently owned peers remain
  usable.
  [test?](../crates/wrix-service/tests/proxy_handoff.rs::transferred_owner_loss_cancels_all_generations_without_revival)
- Failed preparation/transfer cannot route clients to an unready authority,
  restore saved grants after restart or falsely report all-or-nothing rollback.
  [test?](../crates/wrix-service/tests/proxy_handoff.rs::handoff_failures_preserve_fail_closed_ownership)
- Live Podman tooling keeps a slow Blue transfer/CONNECT session while
  subsequent connections from the same unchanged sandbox use Green; launcher
  death then revokes both paths without affecting a peer launch.
  [system?](verify:proxy.planned-handoff-podman)
- Live Apple tooling satisfies the same unchanged-sandbox routing, concurrent
  old/new stream ownership and peer-safe revocation through actual guest TCP
  routes, not DNS-only switching or assumed cross-VM socket transport.
  [system?](verify:proxy.planned-handoff-apple)

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
- Live TCP/SSH/CONNECT stream migration, forced stream closure during normal
  update, or transparent upgrades of every stable forwarding/control component.
  Planned handoff transfers only still-live authorization and future
  connections.
