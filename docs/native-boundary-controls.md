# Native boundary controls

The qualification-only observer selects `wrix-native-test boundary serve`,
`boot-id` and `stop`. All three root-only service operations require the native
service marker and refuse agent images. Snapshot and pinned-daemon signaling
still select the original Python helper; that file remains required.

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

The boot observer preserves the kernel file's exact UTF-8 text and trailing
newline. Shutdown sends only the private token and newline to IPv4 loopback,
requires the exact original `ok\n` acknowledgment, and preserves all
three-second socket bounds. Malformed tokens fail before connecting;
unsuccessful shutdown does not remove evidence.

Boot and shutdown also have original/native acceptance in actual ARM Linux
service-image VMs under the native Apple runtime. The actual Nix-produced
observer image supplies its service marker, packaged owner, Python predecessor
and Rust helper. Boot output is compared with that VM's real kernel, and both
original/native servers acknowledge shutdown and remove their owned files. Each
server gets a separate fresh VM because immediate fixed-port reuse can encounter
TCP TIME_WAIT. The test harness replaces the image entrypoint only to isolate
these operations; it does not claim normal pool initialization, installed-store,
daemon-resource, firewall or containment acceptance. Those broader owners remain
independently required.
