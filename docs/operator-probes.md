# Operator probe boundaries

`wrix-native-test operator-probe` belongs to the standalone test crate, not the
production Wrix, controller, worker, or cache-publisher package. It does not
grant workspace admission or replace a service lifecycle command.

## Historical repair observations

`fixed-repair --root PATH` validates an explicitly selected private snapshot and
prints an inert command plan. Actual read-only observations additionally require
`--read-only --bd ABSOLUTE-PATH --container ABSOLUTE-PATH`. Relative executables
are refused before snapshot reads or subprocess execution.

Snapshot directories and files are pinned, private, and checked against
replacement. CSV, commit identifiers, history, counts, and retained-controller
observations are validated rather than inferred from command success. This
operation does not withdraw SQL rows, checkpoint a live database, restart a
controller, or replay an incident repair.

## Cgroup primitives

Linux `cgroup --root PATH --run` and
`cgroup-mapped --root PATH --util STORE-PACKAGE --run` are explicit operations
for an already qualified, dedicated fixture. They require root, a canonical
cgroup2 mount, a private mount namespace, absent reserved resource names, and
exclusively recognized root-owned bootstrap processes. They are not general host
cleanup commands.

The direct probe checks a 64 MiB/8-PID/half-core builder budget, an actual OOM
kill, and sibling core survival. The mapped probe checks the existing
`0:100000:65536` user/group mapping, immutable 128 MiB/32-PID/half-core ancestor
budgets, delegated subtree ownership, and an actual namespace-UID-30001 job OOM
under a 32 MiB/8-PID/quarter-core budget. Delegation ancestors retain the
original 0755 creation behavior; the separately owned mapped view is
explicitly 0700.

Owned child handles, controller changes, cgroup directories, and private mounts
are unwound in reverse order. Failed cleanup remains an error, including when
another assertion already failed. Namespace actor failures retain their actual
status and diagnostics. Additional failure diagnostics are limited to the
actor's own selected identity, capability, namespace, cgroup, and target-mode
fields, never process arguments or environment dumps.

A successful receipt describes only a cgroup primitive. It is not evidence of
complete Nix, supervisor, application, or unified-service integration.

## Strict Nix requests

`strict-plan --program STORE-EXECUTABLE --marker ABSOLUTE-PATH --system SYSTEM`
prints an explicitly inert derivation plan. The native builder store root is a
real Nix input; Nix interpolation in literal paths is escaped.

`strict-host` requires explicit absolute container-runtime and Nix executables,
the immutable Linux native-test executable, the Linux system, and `--run`. It
targets only the original `wrix-builder` runtime. `strict-guest` is the matching
explicit guest operation. Neither changes daemon configuration or repairs
namespace, account, capability, or mapping prerequisites.

The request preserves `sandbox = true`, `sandbox-fallback = false`, one build
job, and a 90-second command limit. Its outside marker is created exclusively,
mode 0600, only below a canonical owned non-shared directory. Cleanup verifies
the marker inode and preserves a replacement rather than deleting foreign data.
The Rust derivation builder requires a genuinely non-root identity, an absent
outside marker, and a new absolute output. Observations retain the real command
exit status and byte-exact stdout/stderr; command refusal is not success.

A kernel fixture's refusal to provide Nix sandbox namespaces is a real negative
observation. Do not follow a diagnostic suggestion to disable sandboxing or
relax guards to turn that observation into acceptance.

## Network plans and workers

Linux `network-plan` emits ordinary and SHA-256-flat fixed-output derivations
with separately declared immutable native-test and explicit NSS certificate
bundle inputs. Its `native_execution` field is false. Successful Nix evaluation
of the plan is metadata validation, not network or namespace execution.

The `network-worker` primitives use an explicit certificate bundle, no ambient
proxy, bounded certificate and response reads, credential-free transport URLs,
non-root execution, exact expected content hashes, and private-service negative
controls. These workers and plans do not create private network namespaces,
start a private Nix daemon, pivot a root, provision signing authority, or prove
untrusted source transport admission. Those are separate owning acceptance
boundaries.

## Owned network primitive

Linux
`network-fence --ip STORE-EXECUTABLE --nft STORE-EXECUTABLE --setpriv STORE-EXECUTABLE --ca STORE-CERTIFICATE --proc PRIVATE-PROC-MOUNT --run`
requires an already qualified, idle, root-owned fixture and private mount
namespace. Executables and certificate inputs resolve to root-owned,
non-group/world-writable store files; commands use their canonical paths, not
mutable caller aliases.

The probe creates its own `wrix-native-build` namespace, veth pair, NAT table,
resolver files, and bounded private HTTP fixture. Named network namespaces use
iproute2's `/var/run/netns` location; reserved names are checked there and below
`/run/netns`, without assuming the directories alias. Existing foreign entries
are neither removed nor hidden.

A genuinely namespace-UID-30001 worker must fetch the expected public TLS
payload and fail to reach the private service. An exactly parsed, positive nft
packet counter independently confirms the private-service drop. The parent
checks the private service positively before the worker runs.

Inode, interface-index, table-handle, and namespace receipts constrain reverse
cleanup. Changed resources, changed forwarding values, and unconfirmed creation
remain errors; foreign data is preserved. The result describes only the native
unprivileged network primitive, not an actual Nix fixed-output derivation,
private-daemon orchestration, signed input transport, or unified-service
acceptance. The original private probes remain retained until those owning
boundaries are accepted.
