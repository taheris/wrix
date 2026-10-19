# Claude stdio clients

`wrix spawn --stdio --spawn-config spawn.json` selects Claude's stream-json
transport when the supplied immutable `ProfileConfig` selects a Claude image.
Wrix supplies
`--print --verbose --input-format stream-json --output-format stream-json`. It
does not enable the permission control protocol by default.

Clients that support Claude's stdio permission protocol, including Loom, can opt
in through the existing supported `SpawnConfig.agent_args` interface:

```json
{
  "workspace": "/absolute/host/repository",
  "env": [],
  "agent_args": ["--permission-prompt-tool", "stdio"],
  "mounts": []
}
```

Keep the client's other agent arguments in that array. These are separate argv
values, not a shell command. No new `ProfileConfig` fields, environment
variables, or entrypoint customization are needed. `wrix run` also forwards its
agent argv tail, but the permission protocol is intended for a bidirectional
stream-json client, not an interactive terminal.

## Compatibility and security

The flag is a Claude CLI interface, not a Wrix permission broker. A client
opting in must understand the Claude version's control requests and send its
responses on stdin while continuing to consume stdout. A client that only reads
text or ordinary stream-json messages may stall on unsupported requests. Leave
the option absent for such clients. An older or consumer-selected Claude package
may not support the flag; validate against the actual package in the selected
image. Wrix forwards the option unchanged and preserves Claude's failure status
rather than silently retrying without it.

Wrix currently supplies `--dangerously-skip-permissions` in both interactive and
stdio Claude launches. Adding the prompt-tool flag does **not** remove that
bypass or make every tool call require host approval. Do not treat this
transport option as a security boundary or as an approval audit. The
orchestrator controls its responses; sandbox isolation, network policy and
explicit credential grants remain the Wrix security controls. This opt-in does
not change any of them.

Both platform entrypoints have real-process regressions for opt-in argv,
bidirectional stdio, diagnostics and exit status, alongside unchanged-default
argv tests. These use a probe executable; they do not prove a live Claude API
permission exchange or any particular provider's approval semantics.
