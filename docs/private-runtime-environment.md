# Private worker environment transport

Managed launch environment values are not container-runtime `-e KEY=value`
arguments. The launcher writes one private payload into its fresh launch staging
area and mounts only that directory read-only at
`/run/wrix/runtime-environment`. Directory/file modes are `0700`/`0600`. Normal
staging cleanup follows foreground completion; interrupted launchers may leave
private cache data. There is no PID-based retirement claim.

`wrix-runtime-env` runs inside the guest, not as a host-runtime environment
wrapper. It reads the regular private `environment` file, checks the framing,
and execs only an image-pinned target: network bootstrap, agent entrypoint, Krun
relay, or Krun init. Krun therefore gets its terminal dimensions and quoted
command before PTY creation without exposing them in host runtime argv. Shared
Nix endpoint metadata uses the same channel. Host runtime program, working
directory and captured environment fences remain unchanged.

The byte format is `wrix-runtime-env-v1\0` followed by NUL-terminated name/value
pairs. Names are ASCII identifiers; values cannot contain NUL. Empty, multiline,
CRLF, Unicode and binary values retain their bytes. Bounds are 16 MiB total and
131072 records. The host's existing String-based configuration/secret sources
retain their existing Unicode rules. No shell evaluates or exports the file. The
decoder marks the guest overlay ready before exec so the startup scripts do not
recursively decode it. This internal guest marker is not a host admission, trust
or lifecycle receipt; callers cannot supply bootstrap-sensitive names.

Profile images declare `wrix.runtime.environment=1`. The launcher checks that
version before staging and refuses older/unknown images without an argv
fallback. Static profile environment remains non-secret image configuration. The
new transport directory and decoder path join existing protected mount paths;
required/optional policy, selected secret sources and fixed credential mounts do
not change.

The guest ultimately receives environment values, so this does not hide them
from the agent or its own processes. It does not revoke obtained credentials,
encrypt host cache storage, redact arbitrary explicit agent arguments, or alter
host/shared-service trust. Pi's persistent auth directory remains independent.

Acceptance separates the actual launcher's private-file/host-argv producer
boundary from a real native guest executing the pinned decoder against those
same produced bytes. The owning verifier is
`security.runtime-environment-native`; it builds a disposable protocol image,
uses the immutable compiled test binary, verifies real read-only mounts and
empty/multiline preservation, observes the live host runtime command through
`ps`, and leaves unrelated containers alone. Codec, stage permissions, target
exhaustiveness, protected names and incompatible-image refusals also have
ordinary native tests.
