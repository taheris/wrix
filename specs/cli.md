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

Acceptance: [CLI Surface Acceptance](#cli-surface-acceptance).

`wrix` is the single human-facing Wrix CLI. Root parsing, help/error behavior,
global options, and top-level command dispatch are owned here; delegated command
behavior is owned by the sibling spec named in the table.

| Command surface                               | Owning spec   | Purpose                                                                |
| --------------------------------------------- | ------------- | ---------------------------------------------------------------------- |
| `wrix run ...`                                | `sandbox.md`  | Interactive sandbox launch                                             |
| `wrix spawn ...`                              | `sandbox.md`  | Programmatic sandbox launch from `SpawnConfig`                         |
| `wrix service ...`                            | `services.md` | Host-user infrastructure and workspace-scoped service lifecycle        |
| `wrix service dolt ...`                       | `services.md` | Dolt endpoint diagnostics for beads                                    |
| `wrix service image`, `wrix service gc image` | `sandbox.md`  | Runtime-image information and explicit image collection                |
| `wrix service cache ...`                      | `cache.md`    | Project Nix cache operations                                           |
| `wrix beads`, `wrix beads push`               | `beads.md`    | Beads workspace information and explicit session-close synchronization |
| `wrix init ...`                               | this spec     | Repository-local Git, signing, hook, and verification bootstrap        |

Global `--profile-config <file>` remains the explicit launcher configuration
input for `run` and `spawn`, overriding the generated project default under
[Host CLI and Project Configuration](#host-cli-and-project-configuration).
Neither source is a general project or service-administration configuration. The
implementation may use any argument-parsing library or a hand-written parser as
long as the public behavior below holds.

Root help lists every public command group and points detailed behavior to each
command's help. Unknown commands exit non-zero, name the unknown token, and
print enough usage text for the operator to choose a valid command. Historical
standalone entry points such as `wrix-svc`, `beads-dolt`, `beads-push`, and
`<repo>-beads` are not public compatibility surfaces.

### Host CLI and Project Configuration

Acceptance: [Host Configuration Acceptance](#host-configuration-acceptance).

Host consumers provide one Nix-managed `wrix` on PATH, normally through Home
Manager, nix-darwin or an explicit user-profile installation:

```sh
nix profile add 'github:taheris/wrix#wrix'
```

Explicit temporary provision of the host package through Nix is also supported;
no particular profile directory or installation-path heuristic is required.
Installing/updating that package does not activate a service update. The invoked
host package supplies the service-image binding owned by
[Generation Handoff](services.md#generation-handoff).

Projects supply launch data, not another Wrix executable. The recommended
`sandbox.devShell { }` constructor automatically delivers its generated,
immutable launch configuration through a constructor-owned environment pointer
under [Project Launch Handoff](profiles.md#project-launch-handoff). Consumers do
not manually pair image metadata, export configuration paths or add a configured
launcher to PATH. Project configuration cannot select the host service-image
binding.

For example, entering the Wrix and Loom checkouts keeps the same host
`which wrix` result while selecting each project's launch configuration.
`wrix run` uses the selected project image; explicit `wrix service update` uses
the host package's service image for the shared deployment used by both
repositories. Entering a directory or reloading direnv never activates that
update.

Run/spawn use explicit `--profile-config` before the generated project default.
Missing/invalid launch data fails actionably; there is no implicit
profile/image, repository search/evaluation, second binary or latest-version
fallback. Other command groups ignore the project launch pointer; explicitly
passing launcher-only `--profile-config` to them remains an argument error.
Parsing/compatibility and actual-launch checks belong to
[Project Configuration Handoff](sandbox.md#project-configuration-handoff).
Devshell admission and installation guidance belong to
[Project Launch Handoff](profiles.md#project-launch-handoff), including Home
Manager, nix-darwin, `nix profile add` and temporary provision.

Per-project configured `wrix`/`wrix-run` executables and runnable `sandbox-*`
flake targets are not compatibility APIs. Their retirement does not remove
image/configuration outputs, programmatic `spawn`, or explicit invocation of the
host Wrix package through Nix.

### CLI Command Shape

Acceptance: [CLI Shape Acceptance](#cli-shape-acceptance).

Noun/resource/configuration namespaces default to read-only information;
mutations have explicit action verbs. An action verb such as `run`, `spawn`,
`start` or `reset` already identifies an action and does not need a redundant
`apply`/`run` child. Bare `init` is explicitly the verification default of the
init namespace; `init repo` and `init deploy` name its mutating workflows.

Bare `wrix service cache` delegates to passive workspace cache policy/recorded
status under [Cache Commands](cache.md#cache-commands-locking-and-retention).
Bare `wrix beads` delegates to workspace/sync configuration under
[Beads Command Surface](beads.md#command-surface), not to `beads push`. These
defaults do not start services, query SQL or repair state. Existing explicit
domain actions remain unchanged; cache information has no additional `--host`
form beyond the already documented retention control.

Flags select inputs, scope, output or modes when an independent operation would
not make sense as a subcommand. An explicit action may opt into a named
preparation step: `init repo --deploy` composes provisioning with repository
setup, while `init deploy` remains independently usable. This is not a license
for informational defaults to mutate. Redesigned forms need no historical
command/flag aliases; unchanged domain contracts remain owned by their siblings.

Human-readable duration strings use `humantime` parsing rather than a bespoke
per-command grammar. Commands define their meaning and applicable
range/precision limits; invalid or unrepresentable values fail before the
requested action.

### Service Scope

Acceptance: [Service Scope Acceptance](#service-scope-acceptance) and
[Service Update Acceptance](#service-update-acceptance).

Dual-scope operations select the canonical workspace by default. `--host`
selects this host user's shared infrastructure, not a duplicated command branch.

| Command                                      | Scope and effect                                                                                       |
| -------------------------------------------- | ------------------------------------------------------------------------------------------------------ |
| `wrix service`                               | Read-only information about the selected workspace and its services.                                   |
| `wrix service --host`                        | Read-only shared-infrastructure information; no repository required.                                   |
| `wrix service update`                        | Explicit host-user-wide generation/backend update; no repository or `--host` required.                 |
| `wrix service start`                         | Ensure compatible shared infrastructure and start/register the selected workspace's required services. |
| `wrix service stop`                          | Retire only the selected workspace's services and proxy grants.                                        |
| `wrix service stop --host`                   | Shut down shared infrastructure for all this user's workspaces; no repository required.                |
| `wrix service gc [--wait <duration>]`        | Collect selected-workspace Dolt data; no Celler collection.                                            |
| `wrix service gc --host [--wait <duration>]` | Collect host-user Dolt/Celler data; no repository required.                                            |
| `wrix service gc all [--wait <duration>]`    | Host data plus unused Wrix-image collection; no repository required.                                   |
| `wrix service gc image [--wait <duration>]`  | Unused Wrix-image collection only; no repository required.                                             |

Information never registers workspaces, creates managed state, installs images,
starts/repairs services or issues grants, including when absent/stopped. Host
scope does not select a remote machine or grant elevated authority. Workspace
stop preserves peer namespaces and common-cache availability; host shutdown
affects all service availability and proxy grants but preserves data.

Within `service`, `--host` is admitted for information, stop, data GC and
[Cache Retention Commands](#cache-retention-commands), where both scopes exist.
It does not add host start/logs/endpoints, Dolt or other cache workflows.
Intrinsically shared `update`, image inventory, `gc image` and `gc all` keep
their scope without the flag. Unsupported scope combinations fail before
dispatch, not silent flag ignoring.

[Service GC Scope](services.md#garbage-collection-scope) owns maintenance: bare
GC is workspace Dolt-only, without Celler dispatch/availability checks,
peer-data collection or proxy-grant revocation. `gc --host` collects host-user
Dolt data plus one native host-wide Celler cycle without images. `gc all` adds
exactly the `gc image` collector; image-only work does not register/start data
services. Shared forms work outside repositories. `all` does not include other
users, unrelated images, host `/nix/store` GC, volumes or credentials/keys. The
retired `--image` flag and print-only `service dolt gc` are not alternate
maintenance paths.

[GC Availability](services.md#gc-availability) forbids automatic service startup
for every GC mode. Help makes this explicit: required stopped/unavailable data
services produce incomplete work, not implicit startup or a successful empty
collection. For example, `gc all` can collect eligible images while leaving the
service stopped, then report unavailable Dolt/cache work and exit nonzero.
Positively established empty/inapplicable scopes remain normal no-ops.

[GC Contention](services.md#gc-contention) bounds waiting for required
maintenance access per collection unit, defaulting to 30 seconds, without
bypassing locks or stopping peer work. Help identifies the default and
distinguishes this acquisition wait from full-GC execution: expiry makes that
unit incomplete/nonzero while independently safe work continues, not a
successful protected-resource skip.

Every GC mode accepts the per-run `--wait <duration>` override. For example,
`wrix service gc all --wait 2m` selects a two-minute acquisition budget for each
unit in that invocation. It does not change later defaults, persist policy or
set a running-GC/SQL-statement deadline. The option belongs only to GC, not bare
service/host/image information or unrelated actions. Missing values, unsupported
flags and surplus arguments fail before maintenance dispatch or mutation.

[GC Failure Handling](services.md#gc-failure-handling) applies to every mode:
continue remaining independently safe requested work, then report all failures
and completed work in a final summary. Failed/incomplete work returns nonzero;
empty/protected collections are not errors. Completed deletion is not rolled
back. Invalid arguments or ambiguous invocation scope still prevent deletion.

The duplicated `service host ...` namespace, `service gc host`,
`service status`, `service image gc` and `--image` forms are not compatibility
aliases. Unsupported options, children and surplus arguments fail before
dispatch or mutation. Ordinary `service start` remains the ensure path with a
selected workspace; logs/endpoints remain workspace-scoped.
[Cache Retention Commands](#cache-retention-commands) use the same scope flag.
[Image Commands](#image-commands) address the intrinsically shared runtime
store.

Help and results identify selected workspace versus shared host-user scope,
especially for destructive actions. Lifecycle/replacement belongs to
[Services Lifecycle](services.md#lifecycle) and
[Compatibility and Replacement](services.md#compatibility-and-replacement).
`wrix service stop --host` shuts down all serving/draining generations;
`wrix service start` resumes the selected generation/version, not an implicit
upgrade. Planned selection/routing/retirement follows
[Generation Handoff](services.md#generation-handoff); ordinary workspace or
direnv startup never implicitly authorizes activation or disruption.

### Service Update Command

Acceptance: [Service Update Acceptance](#service-update-acceptance).

```text
wrix service update
```

This explicit action selects this host user's whole managed service deployment,
independently of the current repository. It has no workspace variant or
redundant `--host` flag. Scope does not grant management authority: ordinary
cache/proxy traffic credentials cannot authorize an update.

The command delegates candidate preparation, explicit activation, native backend
handoff and safe generation retirement to
[Generation Handoff](services.md#generation-handoff), including its required
four-role plan regardless of current repository, idle traffic or stopped roles;
there is no per-role selector. It applies the invoked host package's bound
service image through the existing image-source contract, not the project's
launch configuration, an unpinned latest-release lookup, an arbitrary positional
image argument or an update of the host CLI itself. Missing/invalid managed
input fails before live selection changes. Unsupported options, children and
extra arguments fail before image preparation, grant transfer or service
mutation; no force, GC-wait, workspace or host-scope alias is implied.

Update requires an existing managed deployment. If absent, fail before creation
and direct the user to `wrix service start` from a repository. For an existing
stopped deployment, this explicit action starts and activates the candidate; it
does not merely stage an unvalidated stopped selection. Ordinary `start` still
resumes the recorded selection rather than upgrading it.

Help and results identify host-user-wide scope, selected generation/version,
backend transition outcomes and any still-draining Blue work. Successful
activation with all four deployment roles ready may return while Blue retains
old connections; that expected draining is not failure and does not wait for
entire sandbox lifetimes. Report retirement separately rather than claiming Blue
was removed or streams migrated. Explain that safe obsolete-generation cleanup
continues automatically after this command exits; another Wrix invocation or GC
is not needed once drain/ownership conditions hold. Failed/incomplete required
role transitions return nonzero and describe known partial effects without
global rollback, closing protected egress/SQL connections or killing backend
work. Explain the
[backend-specific drain units](services.md#cache-and-compiler-backend-handoff):
clean shutdown may close proven-idle Celler/sccache transports after admitted
work completes, so their native clients may need to reconnect at unchanged
endpoints. This is not permission to interrupt active work or idle SQL sessions.

Ordinary start and actual/cached direnv activation reuse/resume the selected
version and can report update needs; they do not invoke this action or activate
Green implicitly. Bare service information remains passive.

### Image Commands

Acceptance: [Image Command Acceptance](#image-command-acceptance).

```text
wrix service image
wrix service gc image [--wait <duration>]
```

The GC-only `--wait` option follows [Service Scope](#service-scope); inventory
has no wait override.

Bare image provides read-only runtime-image inventory. The explicit `gc image`
action requests cleanup of runtime-unused Wrix-managed images; it is not an
implicit launch/startup side effect. These commands address this host user's
shared runtime image store, work outside repositories and need neither a `host`
token nor a `--host` flag. They do not register a workspace or start the shared
service container. Unrelated/ambiguous images and protected container/in-flight
images remain untouched, including when the command is run inside a repository.
[Runtime Image Cleanup](sandbox.md#image-retention-and-cleanup) owns
installation, protected images and cleanup mechanics. Moving that trigger does
not require retaining the former automatic MRU-eight policy. Temporary
import-reference housekeeping remains part of installation, not a broad
image-pruning request. Help distinguishes information from collection;
unsupported children, flags and extra arguments fail before image mutation.

### Dolt Information

Acceptance: [Dolt Command Acceptance](#dolt-command-acceptance).

`wrix service dolt` is the read-only workspace Dolt summary:
configuration/status, transport, available socket or TCP connection details and
a secret-free, shell-escaped connection command/template. Unavailable or merely
configured endpoints are identified honestly, not presented as authenticated
readiness. [Services Dolt](services.md#dolt-service) owns namespace selection,
endpoint meaning and access boundaries.

This prints information, not an interactive SQL session. It does not start,
register or repair services, mint credentials or change database state. The
explicit `dolt status` and print-only `dolt attach` forms are retired without
aliases; connection information is part of the bare default. The print-only
`dolt gc` form is also retired; explicit maintenance belongs to `service gc` or
`service gc --host`, not the information output. Existing endpoint selectors and
the explicit readiness-wait operation remain available. Unsupported flags,
retired commands and extra arguments fail before dispatch; help is read-only and
documents the default.

### Cache Retention Commands

Acceptance:
[Cache Retention Command Acceptance](#cache-retention-command-acceptance).

```text
wrix service cache retention
wrix service cache retention set <duration>
wrix service cache retention reset
wrix service cache retention --host
wrix service cache retention set <duration> --host
wrix service cache retention reset --host
```

Without `--host`, all three actions select only the current canonical
workspace's private view, including shared identity across its Loom clones. With
`--host`, they select only this host user's common public-dependency view and
work outside repositories. Neither scope acts as a host default for, or bulk
update of, private views.

Bare retention reports selected scope, configured effective duration and
default/override source without state creation, registration, provisioning or
mutation. It does not imply live activation. Set/reset are explicit persistent
policy actions; results identify the selected private/common view. For example,
`set 90d` affects only this workspace; `set 90d --host` affects only common. Set
takes one finite, positive native-representable duration; reset takes none.
Permanent retention/disabled eviction, including zero duration, is not
supported. Invalid values, lossy/overflowing conversion, unsupported options and
extra arguments fail before mutation or service dispatch. Parsed-zero GC waiting
is a separate domain and remains valid.

Set/reset distinguish saved-and-confirmed-active policy from saved for ordinary
startup. They do not start/restart/replace services to apply policy.
Save/application/confirmation failures return nonzero and describe known partial
effects or uncertainty, not success or rollback.

Persistence, scoped default restoration, authority and activation belong to
[Retention Control](cache.md#retention-control). Existing `cache config` remains
a read-only resolved-project-settings report, not a host-policy setter.

### Shared verifier app

Acceptance: [Success Criteria](#success-criteria) and
[Scoped Gate Acceptance](#scoped-gate-acceptance).

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
producing a description. Check- and system-tier affectedness follows
[Scoped Gate Selection and Discovery](#scoped-gate-selection-and-discovery),
using batched queries rather than per-criterion discovery.

### Scoped Gate Selection and Discovery

Acceptance: [Scoped Gate Acceptance](#scoped-gate-acceptance).

Wrix's production `loom gate verify --diff` integration selects
browser-dependent checks by checked, definition-derived inputs in every
requested check/system lane, not by a target-name skip list, a touched-spec-only
heuristic or an arbitrary run-frequency throttle. Wrix-owned browser-dependent
definitions provide honest projections sufficient to exclude unrelated
documentation changes. Owning contracts, source/fixtures, transitive
packaging/image dependencies, shared runner definitions and locked tools remain
included wherever they can affect the assertion; uncertainty is not permission
to discard a dependency.

An unrelated documentation-only diff, such as overview wording in
`docs/README.md`, selects no browser-dependent target. Relevant Playwright/MCP
source, fixtures or owning-contract changes retain the applicable checks.
Lockfile, shared-image and shared-dispatch changes retain all applicable
dependent checks and may require broad coverage. Unrelated opaque targets remain
conservatively selected within the requested stage/tier; malformed declarations
or failed discovery are errors, not unknown-input exemptions or passing
evidence.

Inventory, description and scope discovery do not execute target predicates or
realize browser, sandbox-image or other test runtime closures, including those
of unselected or opaque definitions. Realizing the small discovery entry point
and its own tools is distinct from realizing the targets it describes. For the
unrelated documentation case, neither discovery nor selected execution causes
browser/image realization through an unselected target.

Dependency declarations and the consumed Loom planner must agree at the real
runner boundary; merely adding globs without changing actual selection does not
satisfy this contract. Explicit target invocation and unscoped full verification
retain applicable browser coverage under existing stage, tier, platform and
capability rules. Scope exclusion is not an executed pass or capability skip,
and does not expand push-marker authority; `pre-commit.md` owns marker handling.

### Canonical Host Build Dispatch

Acceptance: [Host Build Dispatch Acceptance](#host-build-dispatch-acceptance).

Wrix-managed host verifier/CI Nix realization delegates to the shared pipeline
owned by [Cache Canonical Host Builds](cache.md#canonical-host-build-pipeline).
The execution definition supplies selected build intent; users do not duplicate
runner/build-artifact roots in cache configuration. `cache.md` owns graph
publication admission and in-flight store protection, not indefinite cache pins
or the verifier result protocol.

Affectedness, stage/tier/capability admission and target selection happen before
selected build-graph discovery. Inventory/description requests do not dispatch
builds or mutate cache registration. Scoped execution does not invoke broad
standalone cache-root discovery as a completion step. Guest-local verification
remains guest execution with pull-only cache access; this integration does not
move it onto a host builder or export host publishing credentials. Existing
public command grammar and verifier result/marker semantics are unchanged.

Actually executed host pre-push Nix checks use this same coordination after
normal hook/marker admission, including configured flake checks and selected
verifier/CI realization outside an active devshell.
[Host Entry Publication](cache.md#host-activation-and-pre-push-publication) owns
cache reconciliation, without converting scoped checks to full verification or
running skipped targets merely to seed cache.

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

Acceptance: [Init Acceptance](#init-acceptance) and
[Tool Cache Control Acceptance](#tool-cache-control-acceptance).

`wrix.toml` is an optional repository-root override file. A repository with
default Wrix behavior does not need the file, and init workflows do not create
it merely to record defaults. When present, it contains policy only — never
private key material, generated host keys, absolute private-key paths,
per-machine cache paths, or secrets.

Policy sections are rooted directly in the file: `[git]`, `[init]`, `[cache]`
and `[tool_caches]`. Unknown sections and keys, including a `wrix` namespace,
fail rather than being ignored. `cache.md` owns Nix-cache policy and precedence;
[Tool Cache Controls](#tool-cache-controls) owns the independent sharing
preference.
[test](../crates/wrix-core/tests/repository_policy.rs::unknown_policy_sections_and_keys_are_rejected)

Supported Git and init keys:

| Key                  | Default                                                                                   | Purpose                                                                                                 |
| -------------------- | ----------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| `git.deploy_key`     | `ProfileConfig.security.deploy_key` when supplied, otherwise `<repo-basename>-<hostname>` | Key name used for deploy/signing key resolution                                                         |
| `git.deploy`         | `false`                                                                                   | Whether sandbox launches receive the selected deploy key; does not provision keys                       |
| `git.sign`           | `false`                                                                                   | Whether Git signing is enabled and sandbox launches receive the selected signing key                    |
| `git.remote`         | `origin`                                                                                  | Git remote used for GitHub repository detection and online verification                                 |
| `init.prek_hooks`    | `true` when `.pre-commit-config.yaml` is present                                          | Whether repo setup configures, and bare init verifies, Wrix's prek hook bundle                          |
| `init.online_verify` | `true`                                                                                    | Whether init performs network verification by default; `false` is repo-policy equivalent to `--offline` |

Key-name precedence is `--key`, then `git.deploy_key`, then supplied
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
[git]
deploy = true
sign = true
```

For init workflows, `--sign` or `--no-sign` overrides `git.sign`, which
otherwise defaults to false; ProfileConfig does not supply signing policy. Bare
verification checks that expected policy; repo setup applies it; provisioning
creates requested key material only. `git.deploy` is a sandbox grant, not key
generation, registration or publication. `init deploy` and `init repo --deploy`
are explicit provisioning requests. The policy reader rejects malformed grants
and the unsupported `sign_commits` spelling.

### Tool Cache Controls

Acceptance: [Tool Cache Control Acceptance](#tool-cache-control-acceptance).

[Profiles](profiles.md#shared-and-private-tool-cache-modes) owns the all-cache
shared/private modes. Repository policy uses one optional boolean:

```toml
[tool_caches]
shared = false
```

An omitted table/key inherits shared mode unless an explicit launch choice is
supplied. `false` selects fresh private launch-local tool caches; `true` selects
the host-user worker-only pool across repositories. The section has no
directory/source selectors, paths or credential fields; unknown keys and
malformed values fail under the strict repository-policy parser.

Interactive overrides are `wrix run --cache` and `wrix run --no-cache`.
Programmatic launch uses optional `SpawnConfig.tool_caches.shared` with the same
boolean meaning.
[Launch Tool Cache Selection](sandbox.md#launch-tool-cache-selection) owns their
parsing, inheritance and argument boundaries. Explicit choice wins over
repository policy in either direction without writing the policy file. The
proposed long `--shared-tool-caches` / `--no-shared-tool-caches` names are not
launcher aliases.

Help describes `--cache` as shared worker tool-cache reuse and `--no-cache` as
private writable tool caches, not disabling caching inside the sandbox. Neither
changes `[cache].enable` or disables Nix/Celler, Beads or proxy services; the
sharing choice does select shared versus local compiler use. These are `run`
launcher options, not global flags or a new environment policy channel.

Restricted Rust requires effective private mode under
[Shared Compiler Selection](profiles.md#shared-rust-compiler-service). For
example, `WRIX_NETWORK=limit wrix run --no-cache /repo` is valid; resolved
shared mode is an actionable pre-preparation error, not an automatic fallback or
policy rewrite. Equivalent inherited private policy or the spawn boolean also
satisfies the rule.

### `wrix init`

Acceptance: [Init Acceptance](#init-acceptance).

All init workflows select the current existing Git worktree and its configured
remote. Missing Git metadata or an unresolved root/remote fails with
remediation; none runs `git init`, creates a GitHub repository or configures
another clone.

| Command                           | Effect                                                                                           |
| --------------------------------- | ------------------------------------------------------------------------------------------------ |
| `wrix init [flags]`               | Verify existing Wrix Git/signing/hooks setup without applying or repairing it.                   |
| `wrix init deploy [flags]`        | Provision/reuse requested GitHub keys and registrations only; do not apply repository setup.     |
| `wrix init repo [flags]`          | Apply repository setup using existing keys, then verify it.                                      |
| `wrix init repo --deploy [flags]` | Complete the same provisioning step as `init deploy`, then apply repository setup and verify it. |

Bare verification makes no persistent managed changes, whether it passes or
fails: no key provisioning, Git/Git-object writes, helper/SSH/bootstrap-file
installation, hook/runner repair, policy creation or service provisioning.
Disposable verification artifacts may be created outside managed repository and
credential state and cleaned up; this is not a claim that network checks produce
no remote logs. Missing or drifted setup is reported, never repaired.

`init deploy` checks its selected key material and GitHub registrations, but
does not require already-installed repo helpers/hooks or run the full repository
verification pipeline. `init repo` never generates missing keys unless
`--deploy` was explicitly selected. In the composed workflow, failed
provisioning prevents repository application. Successful repository application
alone is not success: verification must also pass.

Supported flags and placement:

| Flag                   | Applicability and effect                                                                                                                                                                                |
| ---------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `--key <name>`         | All workflows: select the deploy/signing key name for this invocation.                                                                                                                                  |
| `--remote <name>`      | All workflows: select the remote for GitHub detection/verification.                                                                                                                                     |
| `--sign` / `--no-sign` | All workflows: select expected signing policy; mutually exclusive. Verification only checks it, provisioning only manages the requested keys, and repo setup applies it.                                |
| `--offline`            | Bare verification or repo setup without provisioning: omit network/API checks, retaining required local checks. Invalid for `init deploy` or `init repo --deploy`, including offline repository policy. |
| `--no-hooks`           | Bare verification/repo setup: omit hook checks/setup for this invocation, equivalent to `init.prek_hooks = false`; do not remove or rewrite existing hooks.                                             |
| `--deploy`             | `init repo` only: provision keys before applying/verifying the repository.                                                                                                                              |
| `--force`              | Provisioning workflows only: permit replacement of conflicting local keys/registrations.                                                                                                                |
| `-h`, `--help`         | Every workflow: print applicable help without mutation.                                                                                                                                                 |

Unsupported subcommands, misplaced flags, conflicting modes, missing values and
surplus arguments fail with usage before managed writes or remote mutation.
There is no flat `wrix init --deploy` compatibility alias and no separate
`init verify` or `init provision` alias. `--force` without provisioning is not a
general repair/overwrite switch.

`wrix init repo` writes Git configuration to the shared/common Git config of the
repository where it is invoked, so that repository's linked worktrees inherit
the same transport and signing policy. Loom owns `.loom/integration` as an
independent clone: repo setup in the outer repository neither reads nor mutates
the nested repository, and repo setup inside `.loom/integration` applies and
verifies that clone. Bare verification checks only the selected repository.
Per-worktree config may be added only when Git requires it, and it must not
weaken the common policy.

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

Acceptance: [Init Acceptance](#init-acceptance).

The Git transport and signing helpers are context-aware. They keep repo-local
Git config stable across host checkouts, profile containers, and Git worktrees
by resolving key paths at runtime instead of storing host-only or container-only
private-key paths in Git config. The helper command or trampoline recorded in
Git config is part of the CLI contract because Git executes it outside an
interactive Wrix process.

The deploy/signing-key resolution order, explicit signing policy, strict GitHub
host verification, SSH filesystem permissions, and prohibition on ambient SSH
identities are owned by `security.md` § Credential Surfaces. The helpers
installed by `wrix init repo` implement that policy at Git execution time while
keeping the selected key name and helper locations stable across invocation
contexts. Helper correctness must not depend on whether OpenSSH would otherwise
read `$HOME/.ssh/config` or an effective-user home such as `/root/.ssh/config`.

### Deploy Provisioning

Acceptance: [Init Acceptance](#init-acceptance).

`wrix init deploy` and the preparation step of `wrix init repo --deploy` share
the same GitHub-only provisioning behavior. They detect the selected GitHub
remote and generate a passphraseless ed25519 deploy keypair under
`$HOME/.ssh/deploy_keys/<key-name>`. When signing is enabled by `--sign` or
repository policy, they also provision `<key-name>-signing` as a separate
passphraseless ed25519 signing keypair. Private directory/file permissions are
`0700`/`0600`. The deploy public key is registered with write access on that
repository; the signing public key is registered with the operator's GitHub
account only when signing is enabled. Signing-disabled provisioning manages only
the deploy key.

Standalone provisioning does not write Git config, helpers, known-host
installation, hook/runner bindings or `wrix.toml`; the composed repo workflow
performs repository application only after provisioning succeeds. Neither form
changes sandbox grants. Registration requires online operation, so both forms
reject `--offline` or `init.online_verify = false` before mutation.

Matching local keys and remote registrations are reused. Conflicting material
fails unless `--force` permits replacement of the conflicting keys and
registrations. Existing matching signing registrations, including the historical
`signing-<key-name>` title, remain reusable: dropping CLI aliases does not imply
key rotation or destruction of valid state.

### Verification

Acceptance: [Init Acceptance](#init-acceptance).

Bare `wrix init` verifies existing setup against the selected policy and
invocation options without applying them. `wrix init repo`, with or without
`--deploy`, runs the same verification after application and cannot exit success
if it fails. `init deploy` validates provisioning only, not repository
initialization.

Online verification is the default and proves that a fresh host-side Git
operation uses the Wrix helper, strict host-key checking, pinned GitHub host
keys and the selected deploy key. `--offline` or `init.online_verify = false`
omits network/API calls but retains local config, key presence/permissions,
signing requirements, helper-path stability and enabled hook/runner checks.
Offline success does not assert GitHub reachability or repository authorization.

The verifier runs from the invoked repository root and exercises that
repository's effective Git config. It parses the selected remote as a GitHub
repository and runs `git ls-remote` against its canonical SSH URL, so HTTPS
configuration cannot bypass the Wrix helper and another transport cannot produce
false success. It never redirects into a nested `.loom/integration` clone.
Authentication/authorization failure is distinguished from host-key failure:
reaching authentication proves host-key bootstrap, not successful repository
authorization. Verification diagnoses stale/missing helpers and hooks with
`wrix init repo` repair guidance rather than installing them.

## Success Criteria

### CLI Surface Acceptance

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
- Unsupported flags and surplus arguments on beads, Dolt and service operations
  fail before any subprocess; agent passthrough belongs to the sandbox parser.
  [test](../crates/wrix-cli/tests/arguments.rs::unsupported_arguments_fail_before_any_subprocess)
- Unknown root commands exit nonzero, name the token and print root usage.
  [test](../crates/wrix-cli/tests/cli_surface.rs::unknown_root_command_reports_usage)

### Host Configuration Acceptance

- Production run/spawn select explicit `--profile-config` before the generated
  project default, without requiring a configured wrapper executable.
  [test](../crates/wrix-cli/tests/host_configuration.rs::explicit_profile_config_overrides_project_default)
- Missing or invalid launch configuration fails before preparation without
  repository discovery, an implicit profile/image or another CLI fallback.
  [test](../crates/wrix-cli/tests/host_configuration.rs::missing_or_invalid_launch_configuration_has_no_fallback)
- Non-launch commands ignore the project launch pointer, while explicit
  launcher-only flags remain invalid on those commands.
  [test](../crates/wrix-cli/tests/host_configuration.rs::project_launch_defaults_do_not_configure_service_or_helper_commands)
- Host setup/help distinguishes global CLI ownership from project launch data,
  explains the supported installation/recovery choices and does not advertise
  retired configured-launcher or sandbox runnable APIs.
  [test](../crates/wrix-cli/tests/host_configuration.rs::help_explains_host_cli_project_data_and_explicit_installation)

### CLI Shape Acceptance

- Production bare `service cache` dispatches passive selected-workspace
  information, not a cache/service action or a new host-scoped workflow.
  [test?](../crates/wrix-cli/tests/information_defaults.rs::bare_cache_dispatches_information_without_actions)
- Production bare `beads` dispatches workspace/sync information rather than
  pushing, starting services, querying SQL or repairing state.
  [test?](../crates/wrix-cli/tests/information_defaults.rs::bare_beads_dispatches_information_without_push)

- CLI design distinguishes informational namespace defaults from explicit
  actions, uses flags for appropriate selectors/modes, and documents composition
  without treating every bare verb as read-only or requiring compatibility
  aliases.
  [judge?](../tests/judges/cli-command-shape.md#information-and-actions)
- Human-readable CLI duration inputs use `humantime`, with parse/domain errors
  rejected before actions rather than a separate hand-maintained syntax list.
  [test](../crates/wrix-cli/tests/durations.rs::human_duration_inputs_use_humantime)

### Tool Cache Control Acceptance

- Run/spawn help describes short run flags and the optional owned spawn boolean,
  distinguishes shared/private tool caches from Nix/Celler policy, and documents
  explicit launch > repository > shared-default precedence.
  [test?](../crates/wrix-cli/tests/tool_cache_controls.rs::help_distinguishes_tool_sharing_from_nix_cache)
- Repository `tool_caches.shared` accepts typed optional booleans, preserves
  omission/false, rejects unknown keys/malformed values and remains independent
  of Nix-cache policy.
  [test?](../crates/wrix-core/tests/repository_policy.rs::tool_cache_sharing_policy_is_optional_typed_and_strict)
- Run short flags dispatch sharing choices under prefix/conflict rules, while
  long proposed aliases are not accepted as launcher options; agent passthrough
  after the normal boundary remains unchanged.
  [test?](../crates/wrix-cli/tests/tool_cache_controls.rs::run_cache_flags_follow_prefix_and_conflict_rules)
- One-off choices override repository preferences in either direction without
  creating or rewriting policy or persisting the override.
  [test?](../crates/wrix-cli/tests/tool_cache_controls.rs::one_off_cache_choices_do_not_persist_policy)

- Run/spawn reject shared restricted Rust before preparation and explain the
  private-cache remedy; explicit, inherited and programmatic private choices
  remain valid without disabling Nix/Celler or changing repository policy.
  [test?](../crates/wrix-cli/tests/tool_cache_controls.rs::restricted_rust_requires_effective_private_tool_caches)

### Init Acceptance

- Help for every init workflow describes its effects, flag placement and
  verification/provisioning boundaries without mutating managed state.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::workflow_help_is_complete_and_non_mutating)
- Unsupported init subcommands, retired flat flags, misplaced flags, surplus
  arguments, missing values and conflicting modes fail with usage before writes
  or remote mutation.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::argument_errors_precede_local_and_remote_mutation)
- Every init workflow requires an existing Git worktree and resolvable selected
  remote; no failure creates Git metadata or a GitHub repository.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::workflows_require_existing_repository_and_remote)
- Bare init verifies correctly configured setup without persistent managed
  changes, including Git objects, keys, helpers, hooks, runner/SSH files, policy
  and service state, on both success and failure.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::bare_verification_never_applies_or_repairs_state)
- Bare init reports missing/drifted setup with repo-repair guidance instead of
  creating keys or repairing helpers, hooks or bindings.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::verification_reports_drift_without_repair)
- Standalone init deploy provisions/checks keys and registrations without
  applying repository configuration or requiring installed helpers/hooks.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::standalone_deploy_only_provisions_keys)
- Init repo applies requested Git/signing/hooks setup and verifies it, but never
  provisions missing keys without the explicit deploy modifier.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::repo_applies_and_verifies_without_implicit_provisioning)
- Init repo --deploy uses standalone provisioning semantics before application;
  provisioning failure prevents application, and later verification failure
  cannot produce success.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::composed_repo_orders_provision_apply_and_verify)
- Init flag placement follows the workflow table, including force only during
  provisioning and passive no-hooks behavior only on verification/repo setup.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::workflow_flag_applicability_is_enforced)
- Explicit offline mode rejects both provisioning forms before key, API, config
  or policy mutation.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::offline_provisioning_is_rejected_before_mutation)
- Offline repository policy rejects both provisioning forms before mutation.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::offline_policy_rejects_provisioning_before_mutation)
- Valid workflows need no wrix.toml and do not create one to record defaults;
  key selection uses flag > repository > supplied ProfileConfig > derived
  default, and remote/hooks/online policy uses flag > repository > defaults.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::defaults_and_overrides_do_not_create_policy)
- Repository policy accepts root-level Git/init sections, dotted keys and inline
  tables without a redundant namespace.
  [test](../crates/wrix-core/tests/repository_policy.rs::supported_key_remote_and_init_overrides_are_typed)
- Prefixed/mixed policy and wrong-typed ProfileConfig security/key identity fail
  before any workflow mutation.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::invalid_policy_and_profile_identity_precede_mutation)
- Repo initialization applies context-stable transport/signing only to the
  invoked repository, leaving an independent nested integration clone unchanged.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::outer_repo_setup_does_not_mutate_integration_clone)
- Repo initialization inside the independent integration clone applies/verifies
  that clone without mutating the outer repository; linked worktrees share the
  selected repository's common configuration.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::clone_and_linked_worktree_scope_is_preserved)
- With HOME and effective-user home differing, transport obeys security.md's
  credential-resolution, pinned-host, SSH-permission and ambient-identity rules.
  [test](../crates/wrix-cli/tests/init_git_bootstrap.rs::strict_context_aware_ssh_helper)
- A weakening worktree-local transport override makes bare verification fail
  without repairing the override or weakening common policy.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::verification_rejects_worktree_transport_override)
- Repository Git grants default to false, remain independent, and key identity
  alone does not grant credentials.
  [test](../crates/wrix-cli/tests/init_config.rs::git_grants_are_independent_and_default_false)
- Malformed grants and the unsupported sign_commits spelling are rejected.
  [test](../crates/wrix-cli/tests/init_config.rs::invalid_and_retired_git_grants_are_rejected)
- Repo initialization leaves signing disabled by default; opting in configures
  signing and produces a verifiable disposable test signature/commit without
  changing sandbox grants.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::repo_signing_is_opt_in_and_verified)
- Missing required signing material fails instead of silently provisioning it.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::required_signing_material_is_not_implicitly_provisioned)
- Sign/no-sign overrides repository signing policy without rewriting wrix.toml;
  repo applies the choice, bare init only verifies the expected state.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::signing_modes_apply_or_verify_without_rewriting_policy)
- Both provisioning entrypoints create separate passphraseless ed25519 deploy
  and signing keys with secure permissions when signing is enabled, registering
  repo write access and the operator's signing key respectively.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::both_provisioning_forms_pair_requested_signing_keys)
- Signing-disabled provisioning creates only the deploy key and neither writes
  nor implicitly enables sandbox grants; a grant alone never provisions keys.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::provisioning_does_not_infer_or_write_grants)
- Matching local/remote deploy keys are reused without remote mutation.
  [test](../crates/wrix-cli/tests/init_deploy.rs::matching_deploy_keys_are_reused)
- Matching historical signing registrations remain reusable without rotation.
  [test](../crates/wrix-cli/tests/init_deploy.rs::legacy_setup_signing_registration_is_reused_without_rotation)
- Local key conflicts fail unless force permits replacement and registration.
  [test](../crates/wrix-cli/tests/init_deploy.rs::local_key_conflict_requires_force)
- Remote registration conflicts fail unless force permits replacement.
  [test](../crates/wrix-cli/tests/init_deploy.rs::remote_key_conflict_requires_force)
- Provisioning rejects non-GitHub remotes before key or API mutation.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::unsupported_provisioning_remote_precedes_mutation)
- Online verification exercises real Git and the existing strict helper from the
  invoked repository against the selected GitHub SSH URL, including HTTPS
  origins and an independent nested integration clone.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::online_verification_uses_invoked_repository_and_strict_helper)
- Non-GitHub online verification fails before network verification or mutation.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::unsupported_verification_remote_precedes_network)
- Online verification distinguishes host-key failures from authentication or
  repository authorization failures.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::verification_distinguishes_host_keys_and_authorization)
- Explicit offline verification retains local helper/key/signing/hook checks
  without network calls or repair.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::offline_verification_retains_local_checks)
- Offline policy retains the same local checks without network calls or repair.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::offline_policy_retains_local_checks)
- Offline verification rejects insecure deploy-key permissions.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::offline_verification_rejects_insecure_permissions)
- Repeated standalone provisioning preserves matching keys/registrations and
  leaves repository setup untouched.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::standalone_provisioning_is_idempotent)
- Repeated composed repo initialization avoids key, config, hook, helper,
  Git-object and file-metadata churn or remote mutation.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::composed_repo_initialization_is_idempotent)
- Packaged repo initialization supplies the Nix-resolved runner even when it is
  absent from PATH.
  [test?](../crates/wrix-cli/tests/prek_runtime.rs::packaged_repo_init_resolves_runner_without_path)
- Packaged repo initialization repairs a stale runner binding before selecting
  the hook bundle and preserves identical configuration on repetition.
  [test?](../crates/wrix-cli/tests/prek_runtime.rs::repo_init_repairs_stale_runner_and_hook_path)
- Repo initialization configures the Wrix prek bundle only when enabled and
  present, respects independent-clone scope, and leaves hooks unchanged when
  disabled by flag/policy.
  [test?](../crates/wrix-cli/tests/init_workflows.rs::repo_hooks_are_optional_and_clone_scoped)

### Scoped Gate Acceptance

- Browser-dependent `verify:` and `test-ci:` definitions expose checked inputs
  derived from their actual resources, including owning contracts and transitive
  dependencies, rather than fabricated empty declarations or target-name
  filters. [system](verify:cli.browser-gate-scope)
- Production diff-scoped verification of unrelated overview documentation
  selects no browser-dependent target in either requested check or system lanes.
  [system](verify:cli.browser-gate-scope)
- Playwright/MCP source, fixture and owning-contract changes retain their
  applicable browser-dependent targets through the production scoped gate.
  [system](verify:cli.browser-gate-scope)
- Lockfile, shared-image and shared-dispatch changes retain every applicable
  dependent browser target rather than gaining an unrelated-change shortcut.
  [system](verify:cli.browser-gate-scope)
- Inventory, description and scope discovery do not execute target predicates or
  realize browser/image/test-runtime closures, even for opaque or unselected
  definitions; unrelated documentation gate execution adds no such realization.
  [system](verify:cli.browser-gate-scope)
- Opaque inputs remain unknown and conservatively selected within requested
  stages/tiers; malformed declarations and discovery failures remain errors.
  [system](verify:cli.browser-gate-scope)
- Explicit target invocation and unscoped verification preserve applicable
  browser coverage and existing stage/tier/platform/capability semantics;
  exclusions neither count as passes nor broaden marker authority.
  [system](verify:cli.browser-gate-scope)

### Host Build Dispatch Acceptance

- Production host `.#verify`/`.#test-ci` selected Nix realization uses shared
  build coordination without a second project-maintained cache-root inventory.
  [system?](test-ci:test-cache-host-build-pipeline)
- Discovery-only requests dispatch no builds or registration changes, and
  selected host realization remains downstream of normal scope/admission without
  unselected runtime realization or changed result/capability/marker semantics.
  [system?](test-ci:test-cache-host-build-pipeline)
- Guest-local verifier execution neither moves to a host builder nor receives
  host publishing credentials as a consequence of shared dispatch integration.
  [system?](test-ci:test-cache-host-build-pipeline)

- Actually executed host pre-push Nix checks reach canonical build coordination
  outside a devshell, without bypassing hook admission or changing result
  semantics. [system?](test-ci:test-cache-pre-push-publication)

### Service Scope Acceptance

- Help documents workspace-default information/stop/GC, explicit `--host`
  selection and intrinsically shared image/all collection, including
  repository-independent host operations and the all-workspace shutdown effect.
  [test?](../crates/wrix-cli/tests/host_service.rs::host_scope_flag_help_and_argument_boundaries)
- Bare service with and without `--host` dispatches read-only information at its
  scope without registration, provisioning, managed writes, repair or grants.
  [test?](../crates/wrix-cli/tests/host_service.rs::service_information_defaults_are_read_only)
- Retired status/cleanup aliases and duplicated host namespaces, unsupported
  children and extra arguments fail before dispatch or subprocesses.
  [test?](../crates/wrix-cli/tests/host_service.rs::retired_host_namespaces_and_service_aliases_fail_before_dispatch)
- The scope flag is admitted only on documented dual-scope operations;
  unsupported combinations fail before subprocesses or mutation rather than
  adding host workflows or silently changing resource scope.
  [test?](../crates/wrix-cli/tests/host_service.rs::host_scope_flag_rejects_unsupported_combinations_before_dispatch)

- Service help/results distinguish deployment selection, stopped-generation
  resume, new-generation activation and old-generation draining/retirement;
  stop/start and direnv reload are not implicit upgrades.
  [test?](../crates/wrix-cli/tests/host_service.rs::help_distinguishes_resume_activation_and_generation_retirement)

- GC dispatch distinguishes workspace Dolt-only, host Dolt-plus-Celler, host
  data-plus-images and image-only maintenance; shared forms work outside
  repositories and no scope falls back to the print-only Dolt helper.
  [test?](../crates/wrix-cli/tests/host_service.rs::gc_actions_dispatch_dolt_workspace_and_host_celler_scope)
- Retired `service cache prune` and deferred `service cache group ...` forms
  fail before service dispatch, mutation or subprocesses rather than bypassing
  host GC.
  [test?](../crates/wrix-cli/tests/host_service.rs::retired_and_deferred_cache_commands_fail_before_dispatch)
- GC accepts workspace-default data work, `--host` and image/all selectors; the
  retired `--image` flag, unsupported children and surplus arguments fail before
  maintenance begins.
  [test?](../crates/wrix-cli/tests/host_service.rs::gc_modes_reject_retired_flags_before_dispatch)
- The `--wait <duration>` override is accepted for workspace, host, image-only
  and combined GC, and dispatches the chosen acquisition budget without changing
  resource scope or introducing a running-GC deadline.
  [test?](../crates/wrix-cli/tests/host_service.rs::gc_wait_override_dispatches_for_every_mode)
- Missing/surplus wait values or `--wait` as an option on non-GC service
  commands fail before maintenance dispatch, subprocesses or mutation.
  [test?](../crates/wrix-cli/tests/host_service.rs::gc_wait_argument_errors_precede_maintenance)
- GC help documents no automatic service startup and distinguishes unavailable
  required data work from a known-empty collection, including partial `gc all`.
  [test?](../crates/wrix-cli/tests/host_service.rs::gc_help_explains_no_autostart_and_unavailable_units)
- GC help explains the 30-second default and per-run-only `--wait` acquisition
  override, with timeout as incomplete work rather than a whole-command or
  running-GC time limit.
  [test?](../crates/wrix-cli/tests/host_service.rs::gc_help_documents_bounded_contention_wait)
- GC finishes with a summary of completed work and every observed unit failure,
  including partial runs, without implying rollback or reporting only the last
  failure.
  [test?](../crates/wrix-cli/tests/host_service.rs::gc_reports_completed_work_and_all_failures_at_end)
- Failed or incomplete requested work yields nonzero status; empty or normally
  protected collections can succeed, without a failure being hidden by another
  collector's success.
  [test?](../crates/wrix-cli/tests/host_service.rs::gc_exit_status_reflects_failed_or_incomplete_work)

### Service Update Acceptance

- Actual `service update` argv dispatches host-user-wide managed update without
  a repository, current-workspace restriction or extra management authority.
  [test?](../crates/wrix-cli/tests/host_service.rs::service_update_dispatches_host_deployment_without_repository)
- Redundant `--host`, unsupported flags/children, positional images and surplus
  arguments fail before candidate preparation, ownership transfer or mutation;
  unresolved managed image input cannot change live selection.
  [test?](../crates/wrix-cli/tests/host_service.rs::service_update_argument_and_input_errors_precede_side_effects)
- Help identifies the invoked host package as the update-image source, not the
  repository's launch defaults, host-CLI/self update, arbitrary latest lookup or
  implicit activation through start/direnv; it describes the four required
  deployment roles, normal draining and automatic retirement after the command
  exits, without requiring another Wrix invocation or GC. It distinguishes
  protected egress/whole-SQL-session drain from Celler/sccache work drain and
  possible idle-transport closure/reconnection.
  [test?](../crates/wrix-cli/tests/host_service.rs::service_update_help_explains_scope_activation_and_draining)
- Actual update dispatch includes all four deployment roles even when idle,
  stopped or unused by the current repository; gateway-only or partial backend
  readiness cannot produce a successful complete update.
  [test?](../crates/wrix-cli/tests/host_service.rs::service_update_requires_complete_plan_even_for_idle_roles)
- Confirmed activation with all four deployment roles ready succeeds while old
  connections drain, reports selected versus draining/retired generations and
  explains automatic cleanup continuing after the command exits without
  requiring another Wrix invocation or GC; it neither waits for complete sandbox
  lifetimes nor closes protected egress/SQL connections or kills backend work.
  [test?](../crates/wrix-cli/tests/host_service.rs::service_update_reports_successful_activation_with_blue_still_draining)
- An absent deployment makes actual `service update` argv fail before creation
  with guidance to run ordinary `service start` from a repository.
  [test?](../crates/wrix-cli/tests/host_service.rs::service_update_requires_existing_deployment)
- Explicit update of an existing stopped deployment starts/activates the
  candidate and reports readiness or failure rather than only staged state.
  [test?](../crates/wrix-cli/tests/host_service.rs::service_update_activates_existing_stopped_deployment)
- Failed/incomplete required role transitions return nonzero and report actual
  completed/draining/failed phases without false rollback, completed retirement
  or forced old-container removal.
  [test?](../crates/wrix-cli/tests/host_service.rs::service_update_reports_partial_failure_without_global_rollback)

### Image Command Acceptance

- Image commands use the shared host-user runtime store without requiring a
  repository, workspace registration or shared-service startup; cwd does not
  silently narrow or broaden the target pool.
  [test?](../crates/wrix-cli/tests/image_commands.rs::image_commands_use_shared_host_user_store_without_repository)
- Image help exposes bare inventory and points to `gc image` for cleanup;
  retired `image gc` and other invalid child/argument forms fail before image
  mutation.
  [test?](../crates/wrix-cli/tests/image_commands.rs::image_help_and_argument_boundaries)
- Bare image dispatches read-only inventory, while `gc image` dispatches
  image-only maintenance rather than launch/service startup or Dolt/Celler
  collection.
  [test?](../crates/wrix-cli/tests/image_commands.rs::image_information_and_gc_have_distinct_dispatch)

### Dolt Command Acceptance

- Bare dolt renders workspace status, transport, available endpoint details and
  a secret-free connection command without launching an interactive SQL client.
  [test?](../crates/wrix-cli/tests/dolt_commands.rs::bare_dolt_prints_details_without_opening_a_client)
- Dolt help documents the information default; retired status/attach/GC forms,
  unsupported flags and surplus arguments fail before dispatch or mutation.
  [test?](../crates/wrix-cli/tests/dolt_commands.rs::dolt_help_and_argument_boundaries)

### Cache Retention Command Acceptance

- Bare retention reports the selected private/common policy and its source
  without mutation or provisioning; host information works outside repositories.
  [test?](../crates/wrix-cli/tests/cache_retention.rs::bare_retention_reports_selected_scope_without_mutation)
- Help documents workspace-default private retention, explicit `--host` common
  retention and set/reset without implying bulk private-policy updates.
  [test?](../crates/wrix-cli/tests/cache_retention.rs::retention_help_documents_workspace_default_and_host_flag)
- Information/set/reset select the canonical private view without the flag and
  only the repository-independent common view with it.
  [test?](../crates/wrix-cli/tests/cache_retention.rs::retention_scope_flag_selects_private_or_common_for_every_action)
- Set/reset accept exactly their documented arguments; missing/malformed values,
  zero/permanent retention, lossy/overflowing durations, unsupported flags and
  surplus arguments fail before mutation or dispatch; GC-wait zero is unchanged.
  [test?](../crates/wrix-cli/tests/cache_retention.rs::retention_argument_errors_precede_mutation)
- Successful set/reset results identify the affected canonical workspace private
  view or shared common view, not an unrequested wider scope.
  [test?](../crates/wrix-cli/tests/cache_retention.rs::retention_mutation_results_identify_selected_scope)
- Set/reset output distinguishes confirmed active policy from saved-for-startup
  state, while bare configured-policy information does not imply live
  activation.
  [test?](../crates/wrix-cli/tests/cache_retention.rs::retention_results_distinguish_active_and_saved_for_startup)
- Failed retention updates return nonzero and report known partial saved/active
  effects or uncertainty without claiming rollback or success.
  [test?](../crates/wrix-cli/tests/cache_retention.rs::retention_update_failures_report_partial_state)
- Retention help explains live application versus deferred startup and that
  setters do not start/restart/replace infrastructure to apply policy.
  [test?](../crates/wrix-cli/tests/cache_retention.rs::retention_help_explains_activation_without_service_startup)

## Requirements

### Functional

1. **Single public CLI** — `wrix` owns root command parsing, global options,
   help/error behavior, and dispatch to `run`, `spawn`, `service`, `beads`, and
   `init`.
2. **Shared verifier app** — `.#verify` owns the repository-local verifier app
   surface used by `verify:<domain>.<check-id>` annotations, supports batched
   execution plus `--list`, provides the verifier-registry inventory, and is the
   target that runner configuration uses for grouped `verify:` annotations.
   Browser-dependent selection and discovery follow
   [Scoped Gate Selection and Discovery](#scoped-gate-selection-and-discovery).
   Host Nix realization follows
   [Canonical Host Build Dispatch](#canonical-host-build-dispatch).
3. **Delegation boundaries** — `sandbox.md` owns `run`/`spawn` launch semantics,
   `services.md` owns service lifecycle, `cache.md` owns cache semantics,
   `beads.md` owns `beads push` behavior, `pre-commit.md` owns the hook bundle,
   and `security.md` owns credential trust invariants.
4. **Optional config** — `wrix.toml` is read only when present and stores
   override policy only. Defaults must not require a tracked Wrix config file.
   `--no-hooks` is the invocation-scoped form of `init.prek_hooks = false`.
5. **Init workflows** — [Init](#wrix-init) separates read-only verification,
   standalone provisioning and repo application; explicit composition provisions
   before applying/verifying. No workflow creates a Git repository.
6. **Repository scope** — repo setup writes shared/common Git config only for
   the invoked repository. Linked worktrees of that repository inherit its
   policy, while Loom's independent `.loom/integration` clone requires its own
   init invocation and is not silently mutated from the outer checkout.
7. **Context-aware key resolution** — Git helpers implement the credential
   resolution and ambient-identity policy owned by `security.md`.
8. **Git policy** — `git.deploy` and `git.sign` default to false. Signing is
   opt-in for init and sandbox launch; enabled signing requires its key.
   Explicit overrides take precedence over repository policy without persisting
   an override. Key identity and sandbox grants are separate.
9. **Strict GitHub SSH** — Git transport implements the host-verification policy
   owned by `security.md`; this spec owns helper installation and verification
   through `wrix init repo` and read-only `wrix init` respectively.
10. **Deploy provisioning** — [Deploy Provisioning](#deploy-provisioning) pairs
    requested signing keys with deploy keys without changing sandbox grants.
    Both standalone and composed provisioning reject offline mode/policy.
11. **Offline mode** — `--offline` and `init.online_verify = false` disable
    network/API verification only; local config, key, permission, signing,
    helper, and hook checks still run, but offline success does not assert
    GitHub reachability or repository authorization.
12. **Retention control** — information and explicit set/reset use
    workspace-default private scope or explicit host common scope under
    [Cache Retention Commands](#cache-retention-commands); behavior belongs to
    `cache.md`.
13. **Explicit service updates** —
    [Service Update Command](#service-update-command) owns the
    repository-independent `wrix service update` surface; service
    generation/backend behavior belongs to `services.md`, not automatic start or
    direnv activation.

### Non-Functional

1. **Idempotent** — repeated provisioning/repo actions converge without key,
   config, hook, helper, Git-object or metadata churn; bare init never repairs
   or persists changes. See [Init Acceptance](#init-acceptance).
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

- Per-project Wrix executables, `wrix-run` compatibility commands or runnable
  `sandbox-*` flake aliases; project consumers use generated launch data and the
  host CLI.
- Automatic profile installation, implicit latest-package fetching or silently
  disabling Wrix integration when the required host CLI is unavailable.

- Creating Git repositories with `git init`; Wrix initialization configures an
  existing Git worktree rather than creating its Git metadata.
- Compatibility aliases for duplicated `service host ...` / `service gc host`
  namespaces, retired `service status`, `service image gc` or `--image` forms;
  use the documented information defaults, scope flag and GC action tree.
- Interactive Dolt client lifecycle in this CLI cleanup; bare Dolt prints
  connection information instead of opening a session, and status/attach aliases
  are not retained.
- Per-workspace Celler GC, `service cache prune` compatibility aliases and cache
  sharing-group commands/publication; those are not hidden alternate dispatch
  paths.
- Compatibility aliases for retired flat init flags or the unadopted
  `init provision`/`init verify` spellings; bare init is verification.
- Separate signing-key or hook-management command trees in this redesign;
  requested signing-key provisioning remains paired with deploy provisioning.
- Non-GitHub deploy-key provisioning in v1.
- Key rotation cadence beyond explicit `--force` replacement.
- Storing secrets or private-key paths in `wrix.toml`.
- Replacing or reimplementing the upstream `bd` CLI.
- Changing sandbox launch semantics owned by `sandbox.md`.
- Changing service lifecycle owned by `services.md` or cache behavior owned by
  `cache.md`.
- Changing the hook bundle contents owned by `pre-commit.md`.
- Mandating a specific Rust argument-parsing crate.
- Time-based throttles, manual browser-target skip lists or guessing away
  unknown dependencies to make scoped gates appear faster.
- Removing browser coverage from explicit/full verification, changing verifier
  tiers, capability skips or push-marker rules as a scoping shortcut.
- A whole-flake IFD-free guarantee or a mandated relocation of
  generated-manifest assertions to build time; the scoped selection/discovery
  outcome is the contract.
