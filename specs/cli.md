# Wrix CLI

Human-facing command surface, repository initialization, and CLI-level
delegation for Wrix.

## Problem Statement

Wrix needs one predictable command line that works from host shells, devshells,
containers, and Loom-managed clones. The CLI must delegate sandbox, service,
cache, and beads behavior to the specs that own those domains while also
providing a repository bootstrap path that makes Git transport, commit signing,
and hook configuration strict and repeatable outside the devshell.

## Architecture

### Command Ownership

`wrix` is the single human-facing Wrix CLI. Root parsing, help/error behavior,
global options, and top-level command dispatch are owned here; delegated command
behavior is owned by the sibling spec named in the table.

| Command surface          | Owning spec   | Purpose                                                         |
| ------------------------ | ------------- | --------------------------------------------------------------- |
| `wrix run ...`           | `sandbox.md`  | Interactive sandbox launch                                      |
| `wrix spawn ...`         | `sandbox.md`  | Programmatic sandbox launch from `SpawnConfig`                  |
| `wrix service ...`       | `services.md` | Workspace service lifecycle                                     |
| `wrix service dolt ...`  | `services.md` | Dolt endpoint diagnostics for beads                             |
| `wrix service cache ...` | `services.md` | Project Nix cache operations                                    |
| `wrix beads push`        | `beads.md`    | Beads session-close synchronization                             |
| `wrix init ...`          | this spec     | Repository-local Git, signing, hook, and verification bootstrap |

Global `--profile-config <file>` remains the launcher configuration input for
`run` and `spawn`; it is not a general project config file. The implementation
may use any argument-parsing library or a hand-written parser as long as the
public behavior below holds.

Root help lists every public command group and points detailed behavior to each
command's help. Unknown commands exit non-zero, name the unknown token, and
print enough usage text for the operator to choose a valid command. Historical
standalone entry points such as `wrix-svc`, `beads-dolt`, `beads-push`, and
`<repo>-beads` are not public compatibility surfaces.

### Shared verifier app

The repository exposes a developer-facing flake app `.#verify` for Nix-owned and
shell/system-boundary verifier logic. It is not a `wrix` subcommand and is not
part of the human CLI surface. Specs reference its checks through logical
annotation targets of the form `verify:<domain>.<check-id>`; runner
configuration maps those IDs to a batched invocation such as
`nix run .#verify -- <domain>.<check-id> ...`, so many criteria do not spawn one
Nix process each.

`.#verify --list` prints the supported logical IDs and is the authoritative
inventory for the runner's configured verifier registry. Invoking `.#verify`
with one or more IDs runs exactly those checks and reports unknown IDs as
actionable failures. Each ID has a single owner spec: the domain prefix names
the owning area (`profiles.*`, `images.*`, `sandbox.*`, `prek.*`, and so on),
while this spec owns the shared app surface and batching contract.

`--print-inputs [id ...]` on `.#verify` and `.#test-ci` reports a batch
`{"inputs":{"id":["glob", ...]}}` projection of checked resource operands.
Description queries do not execute checks or provision images. Opaque execution
definitions are omitted, retaining Loom's unknown-input always-run behavior;
they are not represented by an empty known input set. Unknown IDs fail before
producing a description. The check runners opt into these batched queries;
system runners remain conservative without queries, avoiding per-scenario
discovery overhead while preserving their execution coverage.
[check](verify:cli.shared-verifier-app)
[check](verify:cli.verify-runner-batching)

### Verifier results and worker acceptance

The shared `.#verify` app and `.#test-ci --json` emit JSON-lines results. Passed
and failed executions use `outcome: "passed"` or `"failed"`, with `target`,
`evidence`, and `execution: {platform, platforms, capabilities}`. `platform` is
the actual canonical architecture/OS token; `platforms` declares applicability
(empty means all platforms), and `capabilities` declares local prerequisites.
Optional fields are omitted, not null.

Declared prerequisites are checked before executing a target. An excluded
platform emits `outcome: "skipped"` and
`skip_reason: {kind: "foreign-platform", reason}`. A missing declared
prerequisite emits
`skip_reason: {kind: "missing-capability", capability, reason}`. Live Wrix
networking checks require `container-runtime`: the platform CLI and, on Linux,
the TUN device and a successful Podman availability probe. A container marker
alone is not evidence of runtime absence. MicroVM checks also require an
accessible KVM device; real Linux firewall checks require user/network
namespaces. These declarations do not apply to tests using fake runtimes, nested
offline container hooks, or assertions that unavailable KVM is rejected.

An exit 77 without a successful prerequisite classification remains an
unauthorized legacy skip (`pass: false`, `skipped: true`, explicit evidence and
execution metadata). Its evidence is never parsed to grant an exemption.
Assertion, invocation, unknown-target, preflight-tool, and build errors remain
failures. A batch exits 0 only for all passes, 77 for skips without failures,
and nonzero other than 77 when any member fails; every member retains its own
result. Direct repeated IDs retain independent invocations; Loom batches unique
requested targets and rejects duplicate/conflicting result records.
[check](verify:cli.shared-verifier-app)

Wrix's check/system runners opt into Loom's `sandbox-capability` policy with an
explicit capability allowlist. Only genuine declared platform or allowlisted
prerequisite gaps can be nonblocking for an otherwise clean worker
`loom gate verify` (`LOOM_INSIDE=1`). Unexpected skips and failures block.
Execution remains skipped, with exit 77 from the producer and tier commands;
worker acceptance is not verified coverage and cannot authorize a push marker.
Outside a worker, permitted skips retain exit 77. Live platform testing remains
in the separate end-of-loop integration-test-branch host stage, not a worker SSH
bridge or per-bead remote-receipt requirement.
[check](verify:cli.shared-verifier-app)

### Optional `wrix.toml`

`wrix.toml` is an optional repository-root override file. A repository with
default Wrix behavior does not need the file, and `wrix init` does not create it
merely to record defaults. When present, it contains policy only — never private
key material, generated host keys, absolute private-key paths, per-machine cache
paths, or secrets.

Supported v1 keys:

| Key                       | Default                                                                                   | Purpose                                                                                                 |
| ------------------------- | ----------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| `wrix.git.deploy_key`     | `ProfileConfig.security.deploy_key` when supplied, otherwise `<repo-basename>-<hostname>` | Key name used for deploy/signing key resolution                                                         |
| `wrix.git.deploy`         | `false`                                                                                   | Whether sandbox launches receive the selected deploy key; does not provision keys                       |
| `wrix.git.sign`           | `false`                                                                                   | Whether Git signing is enabled and sandbox launches receive the selected signing key                    |
| `wrix.git.remote`         | `origin`                                                                                  | Git remote used for GitHub repository detection and online verification                                 |
| `wrix.init.prek_hooks`    | `true` when `.pre-commit-config.yaml` is present                                          | Whether init configures the repo to use Wrix's prek hook bundle                                         |
| `wrix.init.online_verify` | `true`                                                                                    | Whether init performs network verification by default; `false` is repo-policy equivalent to `--offline` |

Key-name precedence is `--key`, then `wrix.git.deploy_key`, then supplied
`ProfileConfig.security.deploy_key`, then `<repo-basename>-<hostname>`. The
ProfileConfig tier supplies only key identity and applies only when the
invocation already has an immutable Wrix profile configuration. Plain host
`wrix init` skips that tier. Remote, hook, and online-verification policy use
explicit CLI flags, then `wrix.toml`, then the defaults in the table. CLI flags
affect the current invocation and do not force creation of `wrix.toml`.

For the independent boolean grants `deploy` and `sign`, precedence is explicit
launch override, then repository policy, then `false`; ProfileConfig supplies
key identity, not grants. `sandbox.md` owns the launch override interface and
`security.md` owns credential exposure and the trust boundary for mutable
repository policy. A repository can configure both once:

```toml
[wrix.git]
deploy = true
sign = true
```

For `wrix init` signing, `--sign` or `--no-sign` overrides `wrix.git.sign`,
which otherwise defaults to `false`; ProfileConfig does not supply signing
policy. `deploy` is a sandbox credential grant, not a request to generate keys,
register them with GitHub, or publish anything. `wrix init --deploy` remains an
explicit provisioning action. The policy reader rejects non-boolean grants and
the unsupported `sign_commits` key rather than treating that spelling as an
alias.

### `wrix init`

`wrix init` is an idempotent repository-local bootstrap. It applies desired
state and then verifies that the resulting Git setup behaves as requested. It
requires a Git worktree and fails with an actionable error when the repository
root or configured remote cannot be resolved.

Supported flags:

| Flag              | Effect                                                                                                                                                                                                           |
| ----------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `--deploy`        | Generate/register the GitHub deploy key and, when signing is enabled, the signing key before applying local config                                                                                               |
| `--key <name>`    | Override the deploy/signing key name for this invocation                                                                                                                                                         |
| `--remote <name>` | Override the remote used for GitHub detection and verification                                                                                                                                                   |
| `--offline`       | Skip network checks and remote API calls; still verify local files, config, helper behavior, and signing preconditions; incompatible with `--deploy` and any `--deploy` invocation selected under offline policy |
| `--sign`          | Enable SSH commit signing for this invocation                                                                                                                                                                    |
| `--no-sign`       | Disable SSH commit signing for this invocation; conflicts with `--sign`                                                                                                                                          |
| `--no-hooks`      | Explicitly skip repo-local `core.hooksPath` setup for this invocation, equivalent to invocation-scoped `wrix.init.prek_hooks = false`                                                                            |
| `--force`         | Replace existing local/remote key material where the selected operation supports replacement                                                                                                                     |

`wrix init` writes Git configuration to the shared/common Git config of the
repository where it is invoked, so that repository's linked worktrees inherit
the same transport and signing policy. Loom owns `.loom/integration` as an
independent clone: init in the outer repository neither reads nor mutates the
nested repository, and init invoked inside `.loom/integration` applies and
verifies policy for that clone. Per-worktree config may be added only when Git
requires it, and it must not weaken the common policy.

Git-executed helper config and other Git-read signing/transport paths must be
context-stable. This includes `core.sshCommand`, `gpg.ssh.program` when used,
and `gpg.ssh.allowedSignersFile`. Each may name a Wrix executable expected on
`PATH` in every supported context, or a trampoline/file resolved from the Git
common directory, but it must not point at a host-only Nix store path, a
container-only path, an absolute workspace path, or an absolute private-key
path.

The applied Git state includes:

- a context-aware Git transport helper selected by `core.sshCommand`;
- context-aware SSH signing configuration when signing is enabled;
- a context-stable allowed-signers file for the selected signing key;
- the platform/context-specific Nix runner binding and Wrix prek bundle as
  `core.hooksPath` when hook configuration is enabled and
  `.pre-commit-config.yaml` exists (runtime resolution and compatibility are
  owned by `pre-commit.md`);
- Wrix-pinned GitHub host keys for SSH verification.

### Context-Aware Git Helpers

The Git transport and signing helpers are context-aware. They keep repo-local
Git config stable across host checkouts, profile containers, and Git worktrees
by resolving key paths at runtime instead of storing host-only or container-only
private-key paths in Git config. The helper command or trampoline recorded in
Git config is part of the CLI contract because Git executes it outside an
interactive Wrix process.

The deploy/signing-key resolution order, explicit signing policy, strict GitHub
host verification, SSH filesystem permissions, and prohibition on ambient SSH
identities are owned by `security.md` § Credential Surfaces. The helpers
installed by `wrix init` implement that policy at Git execution time while
keeping the selected key name and helper locations stable across invocation
contexts. Helper correctness must not depend on whether OpenSSH would otherwise
read `$HOME/.ssh/config` or an effective-user home such as `/root/.ssh/config`.

### Deploy Provisioning

`wrix init --deploy` is GitHub-only in v1. It detects the configured GitHub
remote, generates a passphraseless deploy ed25519 keypair under
`$HOME/.ssh/deploy_keys/<key-name>`, and, when signing is enabled by `--sign` or
repository policy, generates `<key-name>-signing` as a separate passphraseless
ed25519 signing keypair. It registers the deploy public key with write access on
that repository, registers the signing public key with the operator's GitHub
account when signing is enabled, then runs the normal local init and
verification flow. Because remote registration is part of provisioning,
`--deploy` is invalid under any offline policy, whether selected by `--offline`
or by `wrix.init.online_verify = false`.

Existing keys or remote registrations are reused when they match the requested
state. Conflicting existing material fails loudly unless `--force` is supplied,
in which case Wrix may replace the conflicting local keys and remote
registrations.

### Verification

Verification is part of `wrix init`, not a separate best-effort suggestion.
Online verification is the default and proves that a fresh host-side Git
operation uses the Wrix helper, strict host-key checking, the pinned GitHub host
keys, and the selected deploy key. `--offline` or
`wrix.init.online_verify = false` skips network and GitHub API calls but still
checks local config, key presence, permissions, signing requirements, helper
path stability, and hook configuration; it does not claim to prove GitHub
reachability or repository authorization.

The online verifier runs from the root of the repository where init was invoked
and exercises that repository's effective Git config. It parses the configured
remote as a GitHub repository and runs `git ls-remote` against that repository's
canonical SSH URL, so HTTPS configuration cannot bypass the Wrix SSH helper and
a non-GitHub transport cannot produce a false success. It does not redirect
verification into a nested `.loom/integration` clone. Reaching GitHub
authentication or repository authorization without host-key verification failure
is sufficient to prove host-key bootstrap; authorization failure is reported
separately from host-key failure.

## Success Criteria

- Root help and subcommand help expose `run`, `spawn`, `service`, `beads`, and
  `init`, and delegated command help reaches the owning command group.
  [test](../crates/wrix-cli/tests/cli_surface.rs::root_and_subcommand_help)
- Every public root and subcommand flag has a non-blank user-facing description.
  [test](../crates/wrix-cli/tests/cli_surface.rs::public_flags_have_descriptions)
- Public flag descriptions explain current behavior and identify defaults and
  constraints where they are useful to the operator.
  [judge](../tests/judges/cli.sh#test_public_flag_help_quality)
- The packaged `wrix` output installs no legacy `wrix-svc`, `beads-dolt`,
  `beads-push`, or `<repo>-beads` public binaries.
  [check](verify:cli.package-surface)
- `.#verify --list` exposes the supported `verify:<domain>.<check-id>` target
  IDs, and `.#verify <id>...` runs the requested IDs in one process with
  actionable failures for unknown IDs. [check](verify:cli.shared-verifier-app)
- Runner configuration maps `verify:` annotations to the batched `.#verify` app
  invocation rather than spawning one Nix process per criterion, and treats the
  `.#verify --list` inventory as the verifier registry.
  [check](verify:cli.verify-runner-batching)
- Shared result reporting preserves platform/capability skips as exit 77, lets
  failures dominate mixed batches, and never grants worker acceptance to an
  unexpected skip or a real failure. [check](verify:cli.shared-verifier-app)
- `wrix init --help` exits zero without mutating Git config or creating
  `wrix.toml`.
  [test](../crates/wrix-cli/tests/cli_surface.rs::init_help_is_non_mutating)
- Unsupported flags and surplus arguments on beads, Dolt, and service operations
  fail before any subprocess is invoked; agent command passthrough remains owned
  by the sandbox parser.
  [test](../crates/wrix-cli/tests/arguments.rs::unsupported_arguments_fail_before_any_subprocess)
- Unknown root commands exit non-zero, name the unknown token, and print root
  usage.
  [test](../crates/wrix-cli/tests/cli_surface.rs::unknown_root_command_reports_usage)
- A `wrix init` flag missing its required value exits non-zero with usage and
  does not mutate Git config.
  [test](../crates/wrix-cli/tests/cli_surface.rs::missing_init_flag_value_is_non_mutating)
- `wrix init --deploy --offline` exits non-zero with usage before mutating Git
  config.
  [test](../crates/wrix-cli/tests/cli_surface.rs::deploy_offline_flags_are_non_mutating)
- `wrix init --deploy` under `wrix.init.online_verify = false` exits non-zero
  with usage before mutating Git config or policy.
  [test](../crates/wrix-cli/tests/cli_surface.rs::deploy_under_offline_policy_is_non_mutating)
- `wrix init` succeeds without `wrix.toml` and does not create it for default
  behavior. Key-name selection applies flag > repository > supplied
  ProfileConfig > derived-default precedence; remote, hook, and
  online-verification policy apply flag > repository > documented defaults.
  [test](../crates/wrix-cli/tests/init_config.rs::defaults_and_overrides)
- ProfileConfig security policy rejects wrong-typed `security` and
  `security.deploy_key` values before repository mutation.
  [test](../crates/wrix-cli/tests/init_config.rs::profile_config_rejects_wrong_typed_security_policy)
- `wrix init` applies context-stable transport and signing config only to the
  repository where it is invoked and leaves an independent nested
  `.loom/integration` clone unchanged.
  [test](../crates/wrix-cli/tests/init_git_bootstrap.rs::outer_init_leaves_independent_loom_integration_clone_unchanged)
- `wrix init` invoked inside an independent `.loom/integration` clone applies
  and verifies policy for that clone without mutating the outer repository.
  [test](../crates/wrix-cli/tests/init_verify.rs::init_inside_integration_clone_verifies_that_repository)
- With `$HOME` and the effective-user home differing, the Git transport helper
  conforms to the credential-resolution, host-verification, SSH
  filesystem-permission, and ambient-identity policy owned by `security.md`.
  [test](../crates/wrix-cli/tests/init_git_bootstrap.rs::strict_context_aware_ssh_helper)
- An effective worktree-local `core.sshCommand` override that weakens the common
  transport policy makes local init verification fail.
  [test](../crates/wrix-cli/tests/init_verify.rs::worktree_transport_override_fails_verification)
- Repository Git policy defaults both `deploy` and `sign` to false; explicit
  values are independent, and key identity alone does not grant credentials
  [test](../crates/wrix-cli/tests/init_config.rs::git_grants_are_independent_and_default_false)
- The policy reader rejects malformed grants and the unsupported `sign_commits`
  spelling before repository mutation
  [test](../crates/wrix-cli/tests/init_config.rs::invalid_and_retired_git_grants_are_rejected)
- `wrix init` leaves signing disabled by default; `--sign` or
  `wrix.git.sign = true` configures signing and produces a verifiable signed
  test commit
  [test](../crates/wrix-cli/tests/init_signing.rs::signing_is_opt_in)
- Conflicting `wrix init --sign --no-sign` flags fail before repository mutation
  [test](../crates/wrix-cli/tests/cli_surface.rs::conflicting_sign_flags_are_non_mutating)
- Missing fallback signing material is a hard failure when signing is enabled.
  [test](../crates/wrix-cli/tests/init_signing.rs::fallback_signing_key_is_required)
- `--no-sign` explicitly disables commit signing.
  [test](../crates/wrix-cli/tests/init_signing.rs::no_sign_flag_disables_signing)
- Explicit init signing flags override `wrix.git.sign` without rewriting
  repository policy
  [test](../crates/wrix-cli/tests/init_signing.rs::sign_flags_override_repository_policy)
- `wrix init --deploy --sign` generates separate passphraseless deploy and
  signing ed25519 keys with secure permissions, registers the deploy key with
  write access, and registers the signing key with GitHub
  [test](../crates/wrix-cli/tests/init_deploy.rs::explicit_deploy_and_sign_provision_both_keys)
- `wrix init --deploy` with signing disabled provisions only the deploy key and
  does not write sandbox grant policy; setting `wrix.git.deploy` alone does not
  trigger provisioning
  [test](../crates/wrix-cli/tests/init_deploy.rs::deploy_provisioning_does_not_grant_sandbox_credentials)
- Deploy provisioning reuses matching local and remote keys without remote
  mutation.
  [test](../crates/wrix-cli/tests/init_deploy.rs::matching_deploy_keys_are_reused)
- Matching signing registrations with the legacy `signing-<key-name>` title are
  reused without rotating keys or changing remote registration.
  [test](../crates/wrix-cli/tests/init_deploy.rs::legacy_setup_signing_registration_is_reused_without_rotation)
- Conflicting local key material fails unless `--force` replaces it and its
  remote registration.
  [test](../crates/wrix-cli/tests/init_deploy.rs::local_key_conflict_requires_force)
- Conflicting remote key registration fails unless `--force` replaces it.
  [test](../crates/wrix-cli/tests/init_deploy.rs::remote_key_conflict_requires_force)
- Deploy provisioning rejects non-GitHub remotes before remote API mutation.
  [test](../crates/wrix-cli/tests/init_deploy.rs::unsupported_deploy_remote_fails_before_api_mutation)
- Online verification runs real Git from the invoked repository root through the
  configured common-dir trampoline and generated strict SSH helper against the
  GitHub SSH URL derived from the configured remote, even when that remote is
  HTTPS and the repository contains an independent `.loom/integration` clone.
  [test](../crates/wrix-cli/tests/init_verify.rs::outer_init_verifies_its_own_repository_with_independent_integration_clone)
- Online verification rejects a non-GitHub configured remote before mutating
  repository state or attempting network verification.
  [test](../crates/wrix-cli/tests/init_verify.rs::online_verification_rejects_non_github_remote_before_mutation)
- Online verification reports host-key verification failure separately from
  authentication or repository authorization failure.
  [test](../crates/wrix-cli/tests/init_verify.rs::online_failures_distinguish_host_key_from_authorization)
- `--offline` skips network verification while preserving local helper and key
  verification.
  [test](../crates/wrix-cli/tests/init_verify.rs::offline_flag_skips_network_verification)
- `wrix.init.online_verify = false` skips network verification while preserving
  local helper and key verification.
  [test](../crates/wrix-cli/tests/init_verify.rs::offline_config_skips_network_verification)
- Offline verification rejects insecure local deploy-key permissions without a
  network operation.
  [test](../crates/wrix-cli/tests/init_verify.rs::offline_verification_rejects_insecure_key_permissions)
- Repeated identical `wrix init --deploy` runs preserve key, config, hook,
  generated-helper, and Git object state while avoiding file-metadata churn and
  remote mutation.
  [test](../crates/wrix-cli/tests/init_idempotency.rs::repeated_init_does_not_churn_managed_state)
- The packaged launcher supplies the Nix-resolved runner during init even when
  it is absent from PATH
  [test](../crates/wrix-cli/tests/prek_runtime.rs::packaged_launcher_initializes_binding_without_runner_on_path)
- Packaged init installs/repairs the runner binding before selecting the hook
  bundle, including stale installations; repeated initialization preserves
  identical config
  [test](../crates/wrix-cli/tests/prek_runtime.rs::init_repairs_stale_runner_and_hook_path)
- When `.pre-commit-config.yaml` exists and hook setup is enabled, `wrix init`
  points the invoked repository's `core.hooksPath` at Wrix's prek hook bundle
  without mutating a nested integration clone; running init inside that clone
  configures its own hooks. When hooks are disabled by flag or config, init
  leaves hook config unchanged.
  [test](../crates/wrix-cli/tests/init_prek.rs::prek_hooks)

## Requirements

### Functional

1. **Single public CLI** — `wrix` owns root command parsing, global options,
   help/error behavior, and dispatch to `run`, `spawn`, `service`, `beads`, and
   `init`.
2. **Shared verifier app** — `.#verify` owns the repository-local verifier app
   surface used by `verify:<domain>.<check-id>` annotations, supports batched
   execution plus `--list`, provides the verifier-registry inventory, and is the
   target that runner configuration uses for grouped `verify:` annotations.
3. **Delegation boundaries** — `sandbox.md` owns `run`/`spawn` launch semantics,
   `services.md` owns service/cache semantics, `beads.md` owns `beads push`
   behavior, `pre-commit.md` owns the hook bundle, and `security.md` owns
   credential trust invariants.
4. **Optional config** — `wrix.toml` is read only when present and stores
   override policy only. Defaults must not require a tracked Wrix config file.
   `--no-hooks` is the invocation-scoped form of `wrix.init.prek_hooks = false`.
5. **Init apply-and-verify** — `wrix init` applies repository-local Git
   transport, signing, hook, and known-host state, then verifies the result
   before exiting success.
6. **Repository scope** — init writes shared/common Git config only for the
   invoked repository. Linked worktrees of that repository inherit its policy,
   while Loom's independent `.loom/integration` clone requires its own init
   invocation and is not silently mutated from the outer checkout.
7. **Context-aware key resolution** — Git helpers implement the credential
   resolution and ambient-identity policy owned by `security.md`.
8. **Git policy** — `wrix.git.deploy` and `wrix.git.sign` default to false.
   Signing is opt-in for init and sandbox launch; enabled signing requires its
   key. Explicit overrides take precedence over repository policy without
   persisting an override. Key identity and sandbox grants are separate.
9. **Strict GitHub SSH** — Git transport implements the host-verification policy
   owned by `security.md`; this spec owns helper installation and verification
   through `wrix init`.
10. **Deploy provisioning** — `wrix init --deploy` provisions a deploy key and,
    when signing is enabled, a signing key for GitHub repositories, then runs
    the normal init verification path. Provisioning does not enable sandbox
    credential grants. `--deploy` is invalid under `--offline` or
    `wrix.init.online_verify = false` because provisioning requires remote API
    calls.
11. **Offline mode** — `--offline` and `wrix.init.online_verify = false` disable
    network/API verification only; local config, key, permission, signing,
    helper, and hook checks still run, but offline success does not assert
    GitHub reachability or repository authorization.

### Non-Functional

1. **Idempotent** — repeated `wrix init` runs converge on the same state and do
   not churn keys, config, hooks, generated helper files, or Git objects.
2. **Fail-loud** — missing keys, unsupported remotes, permission problems,
   helper failures, and verification failures return non-zero with remediation
   text.
3. **Host/container parity** — each initialized repository's Git config works
   from host shells, devshells, and Wrix containers, including when the
   repository is a Loom integration or bead clone.
4. **No secrets in config** — Wrix config and Git config do not store private
   key material or secrets.
5. **Implementation freedom** — argument-parser library choice, helper language,
   and generated helper file layout are implementation details as long as the
   public contracts hold.

## Out of Scope

- Non-GitHub deploy-key provisioning in v1.
- Key rotation cadence beyond explicit `--force` replacement.
- Storing secrets or private-key paths in `wrix.toml`.
- Replacing or reimplementing the upstream `bd` CLI.
- Changing sandbox launch semantics owned by `sandbox.md`.
- Changing service/cache behavior owned by `services.md`.
- Changing the hook bundle contents owned by `pre-commit.md`.
- Mandating a specific Rust argument-parsing crate.
