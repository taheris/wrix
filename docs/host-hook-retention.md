# Host publication hook retention

A shell's `NIX_CONFIG` can outlive another activation of the same workspace.
Replacing the workspace's current wrapper root does not end that older shell or
its builds. The fixed-output wrapper also does not carry Nix references to its
hook executable, publisher or interpreter.

Host activation therefore retains those four outputs under
`host-nix-roots/generations/<wrapper-store-basename>/`. The top-level `wrapper`
root remains a current-selection pointer; advancing it does not displace a
previous generation's roots. Existing legacy runtime roots are left intact.
Private temporary build directories prevent concurrent activations from
rewriting each other's wrapper source. The imported directory keeps the stable
`hook-wrapper` basename, so identical settings and executables do not create a
new generation merely because a different temporary directory was used.

These are conservative workspace-lifetime roots, not PID leases. Activation
never guesses that an old caller or its inherited build environment has died.
Retiring a generation requires an explicit operator decision after its callers
and builds have finished. This can retain older runtime closures; it is not a
claim of automatic cache retirement or physical storage savings. No service,
profile, database or history roots are removed by activation.

The `services.host-nix-config` verifier includes a private local Nix store test:
real GC preserves two overlapping generations and all six runtime outputs.
Explicitly removing only the old fixture generation's roots lets real GC collect
its wrapper and three runtimes, while the new generation stays valid. Running
this regression against the previous implementation fails at the first GC, which
collects the older generation. Concurrent identical preparations reuse one
wrapper output, and an injected import failure is propagated with temporary
sources cleaned up. The test never runs GC on the host store or replays
production maintenance.

Seven native CLI tests additionally run the actual shell against privately
supervised native controller/Celler fixtures and a private Nix store. They
exercise two real activations, immutable policy snapshots, exact executed
wrapper arguments and private HOME/XDG values, conflicting operator hooks, cache
opt-out, ignored required settings, repository/environment precedence, and
rejection of shared substitution/key trust even with validation disabled. These
check the current host-publication-only contract rather than expecting host
trust in the shared service. Their names begin with `host_publication_hook_`;
the same system owner explicitly runs them through Nextest. The remaining shell
controls compare fake Nix observations with real Nix and exercise stateful
external image inspection/tagging.
