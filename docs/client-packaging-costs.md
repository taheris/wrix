# Consumer client packaging costs

A shell's Wrix client previously used the complete workspace binary output. That
output contains 16 executables, including the host controller, backend services,
image planners and verifier. The consumer wrappers now use a separate Cargo
build selecting only these six binaries:

- `wrix`, `wrix-git-sign`, `wrix-focus-notification`
- `wrix-cache-hook`, `wrix-cache-publish`, `wrix-host-cache-policy`

The complete workspace package still supplies controller preparation, native
fixtures and verification. The initial packaging change shared its dependency
artifact with the client, Clippy and Nextest. The later
[client/server split](client-server-boundary.md) gives the client a
normal-target cache and server-excluding source projection. Full all-target
checks, dependency test compilation and actual Nextest execution remain intact.
No Cargo features, runtime/network checks or security tests were disabled.

`tests/services/client-package.nix` builds the real selected output, checks its
exact executable inventory and executes the CLI.
`tests/services/rust-source.nix` checks that the client and full verifiers
retain independent dependency artifacts, that full dependency checks and Nextest
remain enabled, and that fixture/server edits remain scoped appropriately. The
existing native
`packaged_consumer_shell_replaces_stale_client_before_launch_dispatch` test also
passed against the newly packaged client and its evaluated shell hook, covering
two consumer configurations without provisioning runtimes.

## Bounded measurements

These observations use the same pinned toolchain and dependencies on the
consuming Mac, but are not a controlled CPU benchmark or empty-store campaign.

| Operation                                                          | Wall time | Interpretation                                                                                               |
| ------------------------------------------------------------------ | --------: | ------------------------------------------------------------------------------------------------------------ |
| Earlier full-workspace first shell realization                     |   99.02 s | Rust dependency artifact and workspace output were absent; common Nix/toolchain inputs were present          |
| Experimental selected build-only dependency artifact + client      |  108.08 s | 62 s dependency compilation, 25.73 s client compilation; no demonstrated wall-time win                       |
| Retained narrow client, shared dependency artifact already present |   43.73 s | Three new outputs: client, wrapper and shell; not comparable to dependency-cold 99 s                         |
| Enter minimal fixture shell and run passive service capabilities   |    7.85 s | Client already realized; includes evaluation and first shell-environment output, no Beads/service activation |

The actual executable output decreased from **72,365,864 to 41,852,680 NAR
bytes** (about 42% smaller). That is a binary-package comparison, not the entire
shell closure or its allocated disk use.

The build-only dependency experiment produced 243,948,240 NAR bytes in newly
built outputs, but did not improve observed wall time. That override was
**rejected**, rather than describing fewer Cargo commands as a speedup. The
initial retained implementation shared the existing full dependency artifact.
That decision preceded the actual library split; the new normal-target cache
excludes the server graph rather than merely changing Cargo commands.

The earlier 99 s was not shell-hook latency: build logs show about 23 s of Cargo
checking, 29 s of dependency builds, 1 s of test compilation and 25 s compiling
workspace sources, with the rest in Nix/Crane staging and artifact handling. The
check-format artifacts are useful to the independent verification builds;
removing their construction from one path did not establish a net improvement.

## Independent boundaries

The [client/server library split](client-server-boundary.md) now removes Rama
from the client normal graph without removing server/security verification. Its
bounded cold observation is lower; warm client-edit time is not demonstrably
better. The following costs remain independent.

1. **Selected image catalogue provisioning.** Loom currently retains all nine
   profile/runtime image outputs in its manifest. The Wrix catalogue producer
   (`wx-3qkwi`) and Loom migration (`wx-j8xzf`) address that independent
   fan-out. It is a larger shell dependency boundary than a few redundant
   executable links. The current immutable project configuration can itself
   retain its selected image/digest; do not remove that context without a
   resolver.
2. **Evaluation/activation.** The minimal fixture uses an explicit input
   override and has no committed downstream lock. The 7.85 s observation
   includes lock/evaluation and shell-environment setup overhead; it is not
   production Loom/direnv latency. A locked real consumer and its actual hooks
   need their own bounded trace (`wx-2gag2`), without starting/upgrading shared
   services merely to benchmark them.

No broad image matrix was built for these measurements. The implementation does
not establish a controlled empty-store or real-Loom shell speedup.
