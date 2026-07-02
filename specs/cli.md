# Wrix CLI

Human-facing command surface, repository initialization, and CLI-level delegation for Wrix.

## Problem Statement

Wrix needs one predictable command line that works from host shells, devshells, containers, and Loom-managed worktrees. The CLI must delegate sandbox, service, cache, and beads behavior to the specs that own those domains while also providing a repository bootstrap path that makes Git transport, commit signing, and hook configuration strict and repeatable outside the devshell.

## Architecture

### Command Ownership

`wrix` is the single human-facing Wrix CLI. Root parsing, help/error behavior, global options, and top-level command dispatch are owned here; delegated command behavior is owned by the sibling spec named in the table.

| Command surface | Owning spec | Purpose |
|-----------------|-------------|---------|
| `wrix run ...` | `sandbox.md` | Interactive sandbox launch |
| `wrix spawn ...` | `sandbox.md` | Programmatic sandbox launch from `SpawnConfig` |
| `wrix service ...` | `services.md` | Workspace service lifecycle |
| `wrix service dolt ...` | `services.md` / `beads.md` | Dolt endpoint diagnostics for beads |
| `wrix service cache ...` | `services.md` | Project Nix cache operations |
| `wrix beads push` | `beads.md` | Beads session-close synchronization |
| `wrix init ...` | this spec | Repository-local Git, signing, hook, and verification bootstrap |

Global `--profile-config <file>` remains the launcher configuration input for `run` and `spawn`; it is not a general project config file. The implementation may use any argument-parsing library or a hand-written parser as long as the public behavior below holds.

Root help lists every public command group and points detailed behavior to each command's help. Unknown commands exit non-zero, name the unknown token, and print enough usage text for the operator to choose a valid command. Historical standalone entry points such as `wrix-svc`, `beads-dolt`, `beads-push`, and `<repo>-beads` are not public compatibility surfaces.

### Optional `wrix.toml`

`wrix.toml` is an optional repository-root override file. A repository with default Wrix behavior does not need the file, and `wrix init` does not create it merely to record defaults. When present, it contains policy only — never private key material, generated host keys, absolute private-key paths, per-machine cache paths, or secrets.

Supported v1 keys:

| Key | Default | Purpose |
|-----|---------|---------|
| `wrix.git.deploy_key` | `ProfileConfig.security.deploy_key` when supplied, otherwise `<repo-basename>-<hostname>` | Key name used for deploy/signing key resolution |
| `wrix.git.sign_commits` | `true` | Whether `wrix init` requires and configures SSH commit signing |
| `wrix.git.remote` | `origin` | Git remote used for GitHub repository detection and online verification |
| `wrix.init.prek_hooks` | `true` when `.pre-commit-config.yaml` is present | Whether init configures the repo to use Wrix's prek hook bundle |
| `wrix.init.online_verify` | `true` | Whether init performs network verification by default; `false` is repo-policy equivalent to `--offline` |

Precedence is: explicit CLI flags, then `wrix.toml`, then Nix/ProfileConfig defaults when available, then derived defaults. The ProfileConfig tier applies only when the invocation context already supplies an immutable Wrix profile configuration; plain host `wrix init` skips that tier and derives `<repo-basename>-<hostname>`. CLI flags affect the current invocation and do not force creation of `wrix.toml`.

### `wrix init`

`wrix init` is an idempotent repository-local bootstrap. It applies desired state and then verifies that the resulting Git setup behaves as requested. It requires a Git worktree and fails with an actionable error when the repository root or configured remote cannot be resolved.

Supported flags:

| Flag | Effect |
|------|--------|
| `--deploy` | Generate/register the GitHub deploy key and, unless signing is disabled, the signing key before applying local config |
| `--key <name>` | Override the deploy/signing key name for this invocation |
| `--remote <name>` | Override the remote used for GitHub detection and verification |
| `--offline` | Skip network checks and remote API calls; still verify local files, config, helper behavior, and signing preconditions; incompatible with `--deploy` and any `--deploy` invocation selected under offline policy |
| `--no-sign` | Explicitly opt out of SSH commit signing for this invocation |
| `--no-hooks` | Explicitly skip repo-local `core.hooksPath` setup for this invocation, equivalent to invocation-scoped `wrix.init.prek_hooks = false` |
| `--force` | Replace existing local/remote key material where the selected operation supports replacement |

`wrix init` writes Git configuration to the repository's shared/common Git config when Git worktree layout permits it, so linked worktrees inherit the same transport and signing policy. A Loom driver worktree such as `.loom/integration` therefore uses the same strict Git behavior without a separate init pass. Per-worktree config may be added only when Git requires it, and it must not weaken the common policy.

Git-executed helper config and other Git-read signing/transport paths must be context-stable. This includes `core.sshCommand`, `gpg.ssh.program` when used, and `gpg.ssh.allowedSignersFile`. Each may name a Wrix executable expected on `PATH` in every supported context, or a trampoline/file resolved from the Git common directory, but it must not point at a host-only Nix store path, a container-only path, an absolute workspace path, or an absolute private-key path.

The applied Git state includes:

- a context-aware Git transport helper selected by `core.sshCommand`;
- context-aware SSH signing configuration when signing is enabled;
- a context-stable allowed-signers file for the selected signing key;
- the Wrix prek hook bundle as `core.hooksPath` when hook configuration is enabled and `.pre-commit-config.yaml` exists;
- Wrix-pinned GitHub host keys for SSH verification.

### Context-Aware Git Helpers

The Git transport and signing helpers are context-aware. They keep repo-local Git config stable across host checkouts, profile containers, and linked worktrees by resolving key paths at runtime instead of storing host-only or container-only private-key paths in Git config. The helper command or trampoline recorded in Git config is part of the CLI contract because Git executes it outside an interactive Wrix process.

Deploy-key resolution order:

1. `WRIX_DEPLOY_KEY`, when set and pointing at an existing file.
2. `$HOME/.ssh/deploy_keys/<key-name>`, where `<key-name>` is the selected deploy key name.
3. Fail non-zero. Wrix-managed Git operations must not fall through to the user's default SSH keys, SSH agent identities, or `~/.ssh/config` identities.

Signing-key resolution follows the same rule with `WRIX_SIGNING_KEY` and `<key-name>-signing`. When signing is enabled, a missing signing key is a hard failure; `--no-sign` or `wrix.git.sign_commits = false` is the explicit opt-out.

The transport helper invokes SSH with a Wrix-pinned GitHub known-hosts file and with strict noninteractive options: user SSH config disabled, `BatchMode=yes`, `IdentitiesOnly=yes`, `StrictHostKeyChecking=yes`, and no runtime `ssh-keyscan` or trust-on-first-use fallback. The pinned known-hosts file may live at `/etc/ssh/ssh_known_hosts` in containers or in Wrix-managed repo/Nix state on the host, but it is never learned by appending to the user's `~/.ssh/known_hosts` during init verification.

If Wrix creates or repairs SSH directories or compatibility config files, directory modes are `0700` and `config` / `known_hosts` file modes are `0600`. Helper correctness must not depend on whether OpenSSH would otherwise read `$HOME/.ssh/config` or an effective-user home such as `/root/.ssh/config`.

### Deploy Provisioning

`wrix init --deploy` is GitHub-only in v1. It detects the configured GitHub remote, generates a passphraseless deploy ed25519 keypair under `$HOME/.ssh/deploy_keys/<key-name>`, and, unless signing is disabled, generates `<key-name>-signing` as a separate passphraseless ed25519 signing keypair. It registers the deploy public key with write access on that repository, registers the signing public key with the operator's GitHub account when signing is enabled, then runs the normal local init and verification flow. Because remote registration is part of provisioning, `--deploy` is invalid under any offline policy, whether selected by `--offline` or by `wrix.init.online_verify = false`.

Existing keys or remote registrations are reused when they match the requested state. Conflicting existing material fails loudly unless `--force` is supplied, in which case Wrix may replace the conflicting local keys and remote registrations.

### Verification

Verification is part of `wrix init`, not a separate best-effort suggestion. Online verification is the default and proves that a fresh host-side Git operation uses the Wrix helper, strict host-key checking, the pinned GitHub host keys, and the selected deploy key. `--offline` or `wrix.init.online_verify = false` skips network and GitHub API calls but still checks local config, key presence, permissions, signing requirements, helper path stability, and hook configuration; it does not claim to prove GitHub reachability or repository authorization.

The online verifier must exercise the same Git config path Loom uses from `.loom/integration`-style linked worktrees. A host-side `git ls-remote` that reaches GitHub authentication or repository authorization without host-key verification failure is sufficient to prove host-key bootstrap; authorization failure is reported separately from host-key failure.

## Success Criteria

- Root help and subcommand help expose `run`, `spawn`, `service`, `beads`, and `init`, route delegated commands to their owning crates/specs, and install no legacy `wrix-svc`, `beads-dolt`, `beads-push`, or `<repo>-beads` public binaries.
  [system](bash tests/cli/cli-surface.sh test_root_help_and_legacy_binaries)
- Unknown root commands and malformed `wrix init` invocations, including `--deploy --offline` and `--deploy` when `wrix.init.online_verify = false`, exit non-zero with an actionable error and usage text, while `--help` exits zero without mutating repository state.
  [system](bash tests/cli/cli-surface.sh test_help_errors_are_non_mutating)
- `wrix init` succeeds without `wrix.toml`, does not create `wrix.toml` for default behavior, and applies flag > `wrix.toml` > ProfileConfig > derived-default precedence for key name, signing, remote, hook, and online verification policy.
  [system](bash tests/cli/init-config.sh test_defaults_and_overrides)
- `wrix init` writes shared/common Git config that is inherited by a `.loom/integration`-style linked worktree, and that config contains no absolute host deploy-key path, container `/etc/wrix/keys` private-key path, host-only/container-only helper path, or host-only/container-only allowed-signers path.
  [system](bash tests/cli/init-git-bootstrap.sh test_common_config_inherited_by_loom_integration)
- With `$HOME` and the effective-user home differing, the Git transport helper resolves `WRIX_DEPLOY_KEY` first, `$HOME/.ssh/deploy_keys/<key-name>` second, fails when neither exists, invokes SSH with strict pinned-host-key options without using user SSH config, default identities, or `StrictHostKeyChecking=no`, and leaves any Wrix-created SSH directories at `0700` plus `config` / `known_hosts` files at `0600`.
  [system](bash tests/cli/init-git-bootstrap.sh test_strict_context_aware_ssh_helper)
- SSH commit signing is enabled by default; a missing `<key-name>-signing` key fails hard, `--no-sign` disables signing explicitly, and a signed test commit verifies against the generated allowed-signers file.
  [system](bash tests/cli/init-signing.sh test_signing_required_by_default)
- `wrix init --deploy` generates separate passphraseless deploy and signing ed25519 keys with secure permissions when signing is enabled, registers the deploy key with write access and the signing key with GitHub, reuses matching existing keys, and replaces conflicts only with `--force`.
  [system?](bash tests/cli/init-deploy.sh test_github_deploy_and_signing_keys)
- Online verification runs a fresh host-side Git operation from a minimal Loom-driver-like environment through the Wrix helper and distinguishes host-key verification failure from GitHub auth/repository authorization failure; `--offline` or `wrix.init.online_verify = false` skips network and GitHub API calls while preserving local verification.
  [system?](bash tests/cli/init-verify.sh test_online_and_offline_verification)
- When `.pre-commit-config.yaml` exists and hook setup is enabled, `wrix init` points repo-local `core.hooksPath` at Wrix's prek hook bundle in the same shared config inherited by `.loom/integration`; when hooks are disabled by flag or config it leaves hook config unchanged.
  [system](bash tests/cli/init-prek.sh test_prek_hooks)

## Requirements

### Functional

1. **Single public CLI** — `wrix` owns root command parsing, global options, help/error behavior, and dispatch to `run`, `spawn`, `service`, `beads`, and `init`.
2. **Delegation boundaries** — `sandbox.md` owns `run`/`spawn` launch semantics, `services.md` owns service/cache semantics, `beads.md` owns `beads push` behavior, `pre-commit.md` owns the hook bundle, and `security.md` owns credential trust invariants.
3. **Optional config** — `wrix.toml` is read only when present and stores override policy only. Defaults must not require a tracked Wrix config file. `--no-hooks` is the invocation-scoped form of `wrix.init.prek_hooks = false`.
4. **Init apply-and-verify** — `wrix init` applies repository-local Git transport, signing, hook, and known-host state, then verifies the result before exiting success.
5. **Common worktree inheritance** — init writes shared/common Git config when possible so linked worktrees, including `.loom/integration`, inherit Wrix transport/signing/hook policy.
6. **Context-aware key resolution** — Git helpers resolve env-provided keys first, `$HOME/.ssh/deploy_keys/` keys second, and otherwise fail. Signing keys follow the same rule with `-signing` suffix.
7. **Signing default** — SSH commit signing is enabled by default. Missing signing material is a hard failure unless the operator disables signing explicitly by flag or config.
8. **Strict GitHub SSH** — Git transport uses pinned GitHub host keys, strict host-key checking, batch mode, and identities-only SSH. It never uses trust-on-first-use or ambient user SSH identities.
9. **Deploy provisioning** — `wrix init --deploy` provisions a deploy key and, unless signing is disabled, a signing key for GitHub repositories, then runs the normal init verification path. `--deploy` is invalid under `--offline` or `wrix.init.online_verify = false` because provisioning requires remote API calls.
10. **Offline mode** — `--offline` and `wrix.init.online_verify = false` disable network/API verification only; local config, key, permission, signing, helper, and hook checks still run, but offline success does not assert GitHub reachability or repository authorization.

### Non-Functional

1. **Idempotent** — repeated `wrix init` runs converge on the same state and do not churn keys, config, hooks, or generated helper files.
2. **Fail-loud** — missing keys, unsupported remotes, permission problems, helper failures, and verification failures return non-zero with remediation text.
3. **Host/container parity** — the same repository Git config works from host shells, devshells, Wrix containers, and linked Loom driver worktrees.
4. **No secrets in config** — Wrix config and Git config do not store private key material or secrets.
5. **Implementation freedom** — argument-parser library choice, helper language, and generated helper file layout are implementation details as long as the public contracts hold.

## Out of Scope

- Non-GitHub deploy-key provisioning in v1.
- Key rotation cadence beyond explicit `--force` replacement.
- Storing secrets or private-key paths in `wrix.toml`.
- Replacing or reimplementing the upstream `bd` CLI.
- Changing sandbox launch semantics owned by `sandbox.md`.
- Changing service/cache behavior owned by `services.md`.
- Changing the hook bundle contents owned by `pre-commit.md`.
- Mandating a specific Rust argument-parsing crate.
