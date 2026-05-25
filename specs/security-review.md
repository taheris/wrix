# Security Considerations

Security tradeoffs and mitigations in wrapix, organized around three questions:

- **Boundary** — where is isolation enforced?
- **Policy** — what can the code touch inside the boundary?
- **Lifecycle** — what persists between runs?

## Threat Model

The primary realistic threat for AI sandboxes is **policy leakage**: the agent exfiltrates data through legitimate channels without needing any exploit.

A prompt injection or misbehaving model can use deploy keys + open network + OAuth token + workspace access to exfiltrate code, secrets, or credentials through normal operations. No kernel escape or boundary exploit is required — the agent operates within its granted permissions but uses them for unintended purposes.

This is distinct from boundary escapes (kernel exploits, VM escapes), which are mitigated by the microVM boundary. The component-by-component analysis below addresses both threats, but policy leakage is the more likely one in practice.

**Implication**: defense against policy leakage requires restricting what's available inside the boundary (network, credentials, filesystem), not just strengthening the boundary itself. See [Network Modes](#network-modes) and [Session Transcript](#session-transcript-as-audit-trail).

## Boundary

The boundary is the isolation layer between the sandbox and the host.

### MicroVM Boundary (Both Platforms)

Both Linux and macOS use a microVM boundary — hardware-virtualized isolation with a dedicated kernel per sandbox. This is the strongest practical boundary for AI workloads.

| Aspect | Linux (krun) | macOS (Apple Vz) |
|--------|-------------|-------------------|
| Hypervisor | KVM (open source) | Hypervisor.framework (Apple) |
| VMM | libkrun (Rust, minimal) | Apple container CLI |
| Networking | pasta (userspace) | vmnet (Apple managed) |
| FS sharing | virtio-fs (libkrun) | VirtioFS (Apple) |
| Boundary class | **microVM** | **microVM** |

No boundary-level asymmetry remains. Differences are implementation details.

**Linux: `podman --runtime krun`**

Linux defaults to a container boundary. Set `WRAPIX_MICROVM=1` to opt in to `podman --runtime krun` when `/dev/kvm` exists. The `krun` binary (crun built with libkrun) is bundled via Nix.

`WRAPIX_MICROVM=1` explicitly opts in to a microVM boundary (hardware-virtualized isolation). Requires KVM support and a working krun runtime. Use the container default for cloud VMs without nested KVM, or when GPU passthrough is needed.

**Known krun limitations:**
- ~100MB memory overhead per microVM
- `podman exec` does not enter the VM
- Environment variable passthrough quirks

### Nix Build Sandbox Disabled

Nix's build sandbox is disabled inside containers (`lib/sandbox/image.nix:26-30`).

**Why:**
1. **Nested sandboxing not possible**: Nix's sandbox uses Linux namespaces. Inside a rootless container, these kernel features are restricted. Enabling it fails with permission errors.

2. **Outer container is the security boundary**: The wrapix container provides isolation (rootless execution, user namespace mapping, filesystem isolation). Nix sandbox would be redundant.

3. **Performance**: Sandbox adds overhead with no additional security benefit when the outer container already isolates.

**Blast radius of a malicious flake:**
- Cannot access host filesystem (only `/workspace` mounted)
- Cannot escalate privileges (rootless)
- Cannot persist beyond container lifetime (ephemeral)
- Same access as any other code in the sandbox, including Claude Code itself

**Recommendation**: Only run `nix build` on flakes you trust, just as you should only open projects you trust.

## Policy

Policy defines what the code can touch inside the boundary: filesystem, network, credentials, and syscalls.

### Keys

#### Deploy Keys

Deploy keys enable git push from sandboxed containers. They are generated without passphrases to support automated, non-interactive use by AI agents.

**Tradeoffs:**
- **Convenience**: No passphrase prompt enables autonomous git operations
- **Risk**: If `~/.ssh/deploy_keys` is compromised, keys are immediately usable

**Mitigations:**
- Directory permissions: 700 (owner-only access)
- Key permissions: 600 (private) / 644 (public)
- Keys are repository-scoped (limited blast radius)
- Deploy keys have write access only to the specific repository

**Alternative**: For higher-security environments, manually add passphrases to generated keys and use `ssh-agent` for caching.

#### Builder SSH Keys

The Linux builder generates SSH keys in the Nix store for remote build authentication. These keys are also passphraseless for automated use.

**Why keys are in the Nix store:**
- Same derivation produces same store path, enabling reproducible builds
- `publicHostKey` must be available when nix-darwin evaluates `buildMachines`
- Client key must be readable by root for nix-daemon remote builds

**Tradeoffs:**
- Files in `/nix/store` are world-readable (typically 444/555 permissions)
- On multi-user systems, other local users could theoretically read the private keys

**Mitigations:**
- SSH port bound to localhost only (not network-accessible)
- Password authentication disabled
- Keys are machine-local (not transferred or shared)
- Keys only grant access to the local builder container

**Multi-user risk assessment:** The attack requires a local user to read the key from the Nix store and connect to the builder on localhost. Impact is limited to resource usage (CPU/memory), not data access. For single-user workstations (the typical use case), this is not a concern.

#### Key Rotation

| Key Type | Frequency | Triggers |
|----------|-----------|----------|
| Deploy keys | 90 days | Personnel changes, suspected compromise |
| Builder SSH keys | 180 days | Machine rebuild, suspected compromise |

**Rotating deploy keys:**
```bash
setup-deploy-key -f  # Overwrites existing, updates GitHub
```

**Rotating builder SSH keys:**
```bash
wrapix-builder stop
# Edit lib/builder/hostkey.nix (change comment to invalidate derivation)
nix build
wrapix-builder start
sudo wrapix-builder setup
```

Builder keys are localhost-only, so rotation urgency is lower than deploy keys which have network access to GitHub.

### OAuth Token

The `CLAUDE_CODE_OAUTH_TOKEN` is passed to containers via environment variable for Claude Code authentication.

**Exposure vectors:**
- Other processes in the container can read `/proc/$pid/environ`
- Process listing tools may display environment variables
- Container introspection APIs can enumerate environment variables

**Mitigations:**
- Container runs as a single, non-root user (no other processes expected)
- Token is already present in the host environment (no new exposure surface)
- Container is isolated from host and other containers
- Token is session-scoped with limited validity

**Alternative**: A secrets file mount (`/run/secrets/oauth_token`) could prevent `/proc` exposure but adds complexity for marginal benefit.

### Network Modes

The `WRAPIX_NETWORK` environment variable controls outbound network access:

- `WRAPIX_NETWORK=open` — unrestricted outbound (current behavior, default)
- `WRAPIX_NETWORK=limit` — outbound limited to profile allowlist + base allowlist

**Base allowlist** (always included in `limit` mode):
- `api.anthropic.com` — Claude API
- `github.com` / `ssh.github.com` — git operations
- `cache.nixos.org` — Nix binary cache

**Profile allowlist** — each profile adds its package registries:
- Rust: `crates.io`, `static.crates.io`, `index.crates.io`
- Python: `pypi.org`, `files.pythonhosted.org`

Profiles define `networkAllowlist` alongside existing `packages`, `mounts`, `env` in `lib/sandbox/profiles.nix`.

Open network is a conscious tradeoff: agent autonomy (dependency installation, web research, git push) requires broad network access. The `allow` mode is for users who want tighter policy at the cost of manual allowlist management.

### Resource Limits

**PID limit**: `--pids-limit 4096` prevents fork bombs. Enforced on Linux (podman). On macOS, the Apple `container` CLI has no `--pids-limit` flag; each container runs in its own microVM, so a fork bomb is contained within that VM and cannot affect the host.

**Not enforced:**
- **Disk**: `/workspace` is a host bind mount; capping it requires host-side filesystem quotas, which is outside wrapix's scope.
- **Timeout**: interactive sessions have the user present; loom handles timeouts for orchestrated runs.

### Nixpkgs Channel

Wrapix uses the `nixos-unstable` channel rather than a stable release.

**Why unstable is appropriate:**
- **Ephemeral containers**: Wrapix runs development tasks, not persistent production services
- **Package availability**: Some packages (e.g., `ty`) are unavailable in stable releases
- **Tool currency**: Current linters and formatters are more valuable than backported fixes
- **Container isolation**: The primary security boundary, not package versions
- **Lock file**: `flake.lock` pins specific commits; updates are deliberate

| Package | unstable | nixos-25.05 |
|---------|----------|-------------|
| ty      | 0.0.13   | 0.0.1-alpha |
| ruff    | 0.14.x   | 0.11.x      |
| uv      | 0.9.x    | 0.7.x       |

**Alternative**: Fork the flake and pin to a stable channel, accepting older or missing tools.

## Lifecycle

Lifecycle defines what persists between runs.

### Ephemeral by Default

Containers are ephemeral (`--rm`). Nothing inside the container persists after exit except:
- `/workspace` — bind-mounted from host, persists all workspace changes
- `.wrapix/log/` — session transcripts (see below)

External orchestrators may keep their own state directories under
`.wrapix/<name>/` (e.g. `.wrapix/loom/`); wrapix itself does not own them.

### Session Transcript as Audit Trail

A structured session summary is persisted to `.wrapix/log/` after each wrapix session (interactive or orchestrator-driven).

Each record captures:
- Session timestamp and duration
- Which beads issue was being worked on (if any)
- Files read/written
- Commands executed
- Network-accessing tool calls
- Link to full `.claude/` session data

Orchestrator-driven sessions (signalled by `LOOM_MODE=1`) additionally link the log entry to the bead ID for traceability.

**Rationale**: The agent's own session transcript is a richer audit trail than OS-level logging (strace, process tree) because it includes intent and reasoning, not just syscalls. For the primary threat (policy leakage from misbehaving agent / prompt injection), the session transcript is sufficient. OS-level audit would only matter for an adversarial-agent threat model (agent deliberately hiding actions), which is a different class of problem.

### Directory Layout

All wrapix state lives under `.wrapix/`:

```
.wrapix/
└── log/        # Session transcripts
```

External orchestrators may add their own subdirectories (e.g. `.wrapix/loom/`);
those are out of wrapix's lifecycle.
