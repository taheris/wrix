# Native boundary controls

The qualification-only observer selects `wrix-native-test boundary serve` for
its known-live IPv4/IPv6 loopback controls. The root-only service operation
requires the native service marker and refuses agent images. Other observer
operations still select the original Python helper; its snapshot, boot ID, stop
and pinned-daemon signal paths are not retired by this slice.

The native server preserves the original single-port, dual-loopback protocol:
`probe` returns `ok\n`; a private 32-byte random token, encoded as 64 lowercase
hex digits, acknowledges shutdown and removes only the owned control files and
outside marker. Other connections record peer coordinates and the stripped byte
count without losing later positive controls. Each read remains bounded to 128
bytes with a three-second connection timeout. Existing files and symlinked
control roots are refused; failure evidence is not recursively collected.

The retired Python unit driver previously exercised that real server in a
caller-owned temporary directory. Its original actual-socket scenario passed on
Darwin and through the original Linux observer-owner check before retirement.
Rust library and CLI tests now exercise the actual native server, both IP
families, empty/unexpected input, whitespace, token-only shutdown, marker and
socket cleanup, foreign-file refusal and failed-readiness evidence retention.
Both-platform native packages and the selected Linux observer-owner delegation
check execute these tests. No Python test adapter replaces them.

These are protocol, fixture ownership and observer-delegation checks. They are
not installed-store, daemon-resource, firewall, native VM or containment
acceptance. Those broader owners remain independently required.
