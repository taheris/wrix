# Services VM verifiers

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
external Nix/driver fixtures and actual kernel preflight. `verify:prek.ci-batching`
also exercises runner preparation, mixed-batch failure dominance and Loom's
worker/host consumption. Fixture passes are conformance evidence, not passing
services VM results.
