# Pool-service bootstrap and transport

Run the dedicated image inside a disposable Linux VM:

```bash
nix run .#legacyPackages.x86_64-linux.ciApps.test-shared-nix-pool-bootstrap
```

The verifier consumes the factory image's filesystem and environment. It pivots
away from the test guest's init root and unmounts that root before bootstrap;
chroot alone is unsuitable because it prevents the user namespaces required by
strict Nix sandboxing.

It checks argument, UID, mode, alias and retained-state refusals; inherited Nix
policy isolation; an untrusted native peer's rejected privileged overrides; a
strict unprivileged build with a readable-outside/hidden-inside filesystem
control; native GC preservation; and retained-owner refusal without database
changes. The native cgroup policy limits CPU, memory and PIDs, while one build
user bounds concurrent native builds. A preallocated 8 GiB ext4 backing contains
store data, logs and build scratch space. The verifier exercises disk exhaustion
and a cgroup OOM, then confirms the native daemon and existing output survived.

Build-network policy attaches cgroup BPF hooks before native admission. Its UID
selection uses the guest kernel's build identity, including inside Nix user and
network namespaces. Live controls cover a reachable public-address fixed-output
build and denied IPv4/IPv6 loopback in fixed-output and ordinary builds.

The image also runs a root-owned lease controller and public-key-only SSH on
port 2222. Startup requires a private `bootstrap.json` in the service data root,
containing version `1` and the host administrator's canonical Ed25519 public
key. Administration is a fixed JSON operation, not a shell. Registered launch
keys reach only forced-untrusted native Nix sessions or read-only SFTP inside a
filesystem jail. SSH forwarding, PTYs, user configuration and passwords are
disabled. Native GC exclusion covers outstanding leases; release first revokes
new admission and refuses completion while native clients/builds remain.

The controller duplicates the supervisor's native GC-lock open description,
rather than reopening its pathname. A live fault control stops the supervisor,
kills and observes the controller's death, and confirms independent kernel GC
exclusion remains before allowing fail-stop to proceed. This is not the full
host/worker lifetime-handoff acceptance gate.

Live transport checks cover pinned host authentication, worker-key rejection by
the administrator account, privileged repair and unsigned input-addressed import
denials, fixed-command enforcement, read-only export, a readable outside-jail
control, live-lease GC refusal, release and native collection. Trusted startup
can import an exact staged image archive and pin its declared paths; this is not
a worker signature exception. Management and transport processes share the
aggregate resource budget and have a bounded control subgroup.

This is a guest-service component verifier, not installed-store reuse, host
launch authorization, host-owner disk admission/supervision, lifetime handoff,
run/spawn delivery, or Podman/krun/Apple platform acceptance. Those contracts
have separate gates in `specs/store.md`. Shared launches remain disabled.
