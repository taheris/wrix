# Pool-service bootstrap

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
changes.

This is a bootstrap component verifier, not installed-store reuse, authenticated
worker admission, filesystem export, disk-budget, root-handoff, run/spawn, or
Podman/krun/Apple platform acceptance. Those contracts have separate gates in
`specs/store.md`.
