# Workspace Admission and Host State

Explicit host authorization associates external workspace members with stable
native namespaces.

## Problem Statement

A checkout's location is not permission to share another checkout's services or
native data. Wrix separates external membership attestations from host
admission, preserves existing native namespaces, and keeps host control metadata
outside worker-editable repositories. Service lifecycle, cache publication,
Beads data, credential delivery and deployment ownership retain their separate
contracts.

## Architecture

See [Architecture](../docs/architecture.md) and
[Terminology](../docs/README.md#terminology-index). Acceptance is defined by
[Success Criteria](#success-criteria).

`wrix-core` owns native paths and non-authorizing workspace descriptions.
`wrix-state` depends on core and owns host capability discovery, typed
integration slots/membership, checked selections, resolver execution, admission
and control storage. CLI, service, cache, Beads and sandbox consumers depend on
state; core has no dependency on state. An opaque checked `Context` retains the
original observation and database generation through effects. A `Workspace` or a
computed `WorkspaceHash` alone conveys no operational authorization.

The identities have separate meanings:

- Caller checkout: where this invocation performs caller-specific Git work.
- Git common directory: administrative evidence for an actual Git checkout,
  including linked worktrees; not external membership or service authority.
- Membership: an opaque resolver-supplied identifier within a host-selected
  slot.
- Primary service scope: an attested checkout used for shared service/cache
  configuration and evaluation, not a replacement caller working directory.
- Native namespace key: stable host storage identity selected by admission.
- Operational authority: the checked host context plus each operation's existing
  locks, resource ownership and permission guards.

### Native Control Storage

On Linux, durable control storage is
`${XDG_STATE_HOME:-$HOME/.local/state}/wrix/state.db`, and bulky cache storage
uses `${XDG_CACHE_HOME:-$HOME/.cache}/wrix`. Empty XDG values use these
defaults; nonempty roots are absolute. Darwin uses
`$HOME/Library/Application Support/wrix` and `$HOME/Library/Caches/wrix`
respectively, independent of XDG overrides.

The private, owner-checked SQLite database has an application/schema version and
random generation. Its authority, selected integration, binding, participant and
namespace-metadata records describe host decisions, not live service readiness.
Publication is atomic; mutations are serialized and transactional. Unknown
schema, malformed expected records and a missing expected database fail closed
rather than initializing replacement authority.

Namespace metadata includes service endpoint documents, cache status,
publication manifests, pending candidates and closure-marker metadata. Host
operations store these records only in the database. They do not create or
recreate `services.json`, `cache-status.json`, `publish-roots.json`, `pending/`
or closure-list `gcroots/` files. Worker filesystem projections remain
observations, not alternative host authority. Repository databases are optional
and non-authorizing; none is required by this protocol.

Explicit import admits supported existing metadata without rewriting or deleting
native files. Ordinary host updates and queue/closure retirement leave those
legacy files untouched. Before initial import, quiesce older writers and back up
native state. After checking imported records and service/cache observations,
removal of known obsolete regular metadata files is a separate, explicit
operator action. A repeated import is not evidence that files subsequently
written by an older binary were imported. Never remove keys, actual Nix
retention roots, runtime state or payloads as metadata cleanup.

Secrets and key material, native Dolt/Celler/Nix databases, sockets, runtime
locks, actual retention roots, cache payloads, notification state, controller
deployment state, Git execution/push evidence and repository policy retain their
owners and formats outside this database. Metadata alone is not resource
ownership, liveness or a deletion grant.

## Commands and Authorizations

Acceptance: [Success Criteria](#success-criteria).

The explicit command surface is:

```text
wrix workspace initialize
wrix workspace acknowledge-scope --version 1
wrix workspace integration select --slot SLOT --executable ABSOLUTE_PATH
wrix workspace integration disable
wrix workspace admit --integration SLOT --expect-membership ID --checkout PATH (--allocate | --retain KEY)
wrix workspace import-native --native-key KEY
wrix workspace inspect --checkout PATH
```

Initialization enables admission storage but does not select an integration,
acknowledge standalone scope, import native files or enroll a checkout.
Acknowledgment records independent consent to the versioned standalone scope
rule. Integration selection chooses host-trusted executable code and a stable
slot, not permission to associate its members. Import registers supported native
metadata, not historical checkout provenance or external membership. Only
explicit admission associates a verified member with a namespace.

Without an integration, the nearest actual Git checkout supplies standalone
scope; outside Git, the canonical selected directory supplies scope. Independent
nested clones remain independent regardless of `.loom` names. Previously
validated access and read-only diagnostics are distinct from new standalone
allocation, which requires scope acknowledgment. Disabled integration does not
release existing binding or participant reservations.

Allocate chooses the primary's derived native key only when candidate keys have
no competing native state or reservations. Retain names an explicitly imported,
supported native namespace; it neither moves data nor claims recovered checkout
history. Repeated admission confirms the existing association. Changed primary
observations require explicit readmission while retaining the binding's key.
Forward, reverse and cross-slot conflicts are errors, not merge/rebind requests.

## Resolver Protocol

Acceptance: [Success Criteria](#success-criteria).

The host directly executes the selected absolute executable with argument
`resolve`, a controlled environment and `/` working directory. The adapter is a
read-only attestor, not a namespace allocator, credential issuer or host command
router. Its single JSON request is version 1 with `caller_directory_b64`, the
canonical caller-directory bytes encoded using canonical padded Base64. Stdin
ends after that request.

The single reply has exactly `version`, `membership`, `checkout_root_b64` and
`primary_root_b64`. Version is 1; membership is a nonempty, control-free UTF-8
identifier of at most 1024 bytes. Roots encode absolute canonical Unix-native
paths. Duplicate/unknown fields, trailing data, unsupported versions and lexical
aliases are invalid. Git discovery independently verifies real checkout and
common-directory administration; the reported caller checkout contains the
actual caller directory. The adapter supplies no native key or permissions.

Each output stream is limited to 16 KiB. Adapter and Git subprocess execution
share one 30-second deadline; overflow, interruption and execution failure
refuse admission. Process-group cleanup covers ordinary descendants and
inherited pipes, not deliberately detached descendants or uninterruptible kernel
operations.

## Success Criteria

- Workers cannot initialize or obtain host authority by changing HOME or
  invoking the library directly.
  [test](../crates/wrix-cli/tests/workspace_state.rs::worker_cannot_initialize_host_authority_through_cli_or_library)
- Initialization creates a private database without acknowledging standalone
  scope.
  [test](../crates/wrix-cli/tests/workspace_state.rs::initializes_private_database_without_acknowledging_scope)
- Acknowledgment is independent and idempotent; inspection does not mutate
  state.
  [test](../crates/wrix-cli/tests/workspace_state.rs::acknowledgment_is_independent_idempotent_and_inspection_is_read_only)
- Absent-store inspection creates nothing.
  [test](../crates/wrix-cli/tests/workspace_state.rs::absent_store_inspection_creates_nothing)
- Missing expected or unsupported databases are not reset.
  [test](../crates/wrix-cli/tests/workspace_state.rs::malformed_or_missing_expected_database_is_not_reset)
- Concurrent initialization publishes one valid database.
  [test](../crates/wrix-cli/tests/workspace_state.rs::concurrent_initializers_publish_one_database)
- Admission preserves caller/primary separation and confirms repeated admission.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::integrated_admission_preserves_caller_and_primary_scope_and_is_idempotent)
- Integration disablement preserves native-key reservations.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::disabling_integration_does_not_release_a_native_key)
- Changed primary scope requires readmission without rekeying.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::changed_primary_requires_readmission_and_retains_the_key)
- Native import is explicit; retention makes no historical provenance claim.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::native_import_is_explicit_and_retention_does_not_claim_old_checkout_provenance)
- Unknown membership refuses ordinary service/cache/init effects.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::ordinary_service_cache_and_init_refuse_unknown_membership_before_effects)
- Resolver execution has no repository working directory or inherited Git
  routing.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::adapter_execution_has_no_repository_working_directory_or_inherited_git_overrides)
- Strict reply parsing refuses malformed attestations without changing state.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::resolver::strict_resolver_rejects_unknown_duplicate_trailing_version_and_noncanonical_fields)
- Subdirectory requests preserve the actual caller and deliver EOF.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::resolver::resolver_request_preserves_subdirectory_and_finishes_with_eof)
- Native non-UTF-8 paths survive command and protocol boundaries without loss.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::resolver::non_utf8_checkout_paths_roundtrip_through_cli_and_protocol_without_loss)
- Linked worktrees are verified through actual Git administration.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::resolver::linked_worktree_administration_is_a_real_checkout_participant)
- Either stream overflowing refuses without admission changes.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::resolver::stdout_and_stderr_overflow_refuse_without_state_changes)
- Descendant-held pipes do not escape the execution deadline.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::resolver::descendant_held_pipes_do_not_escape_the_overall_execution_deadline)
- Interrupting a resolver cleans up without mutating admission.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::resolver::interrupt_cleans_up_the_resolver_without_mutating_admission)
- Concurrent identical admissions confirm one key and binding.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::hardening::concurrent_identical_admissions_confirm_one_key_and_one_binding)
- Forward, reverse and cross-slot reservations refuse conflicting associations.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::hardening::retention::native_key_and_participant_reservations_refuse_forward_reverse_and_cross_slot_conflicts)
- Previously admitted participant residue blocks ordinary access and
  readmission.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::hardening::previously_admitted_participant_residue_blocks_primary_operations_and_readmission)
- Fresh changed attestations refuse service files and launch journals after
  initial selection.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::hardening::changed_attestation_is_rechecked_before_service_files_are_created)
- Readmission does not repair malformed expected binding records.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::hardening::malformed_expected_binding_records_are_not_repaired_by_readmission)
- Initialization leaves existing native metadata untouched and unimported.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::hardening::initialization_does_not_import_existing_native_metadata_or_acknowledge_scope)
- Host service-name collision checks use recorded metadata despite export
  deletion.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::hardening::collision_checks_use_recorded_services_after_export_deletion)
- Host cache status uses database authority and remains read-only.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::host_cache_status_uses_database_not_export_files_and_is_read_only)
- Removed flat-cache pruning refuses without altering imported closure records,
  retained archives or keys.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::hardening::retention::unsupported_flat_cache_prune_preserves_imported_closures_and_keys)
- Host service and cache commands persist control records without creating
  redundant JSON, pending or closure-list exports; secrets and locks stay
  native.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::hardening::consolidation::host_service_and_cache_commands_create_database_records_without_metadata_exports)
- Host updates and queue/closure retirement preserve explicitly imported files.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::hardening::consolidation::host_mutations_preserve_imported_files_while_retiring_database_pending_and_closures)
- The actual host hook selects the publishing owner and queues authoritative
  pending work under contention without creating metadata export files.
  [test](../crates/wrix-cache/tests/host_hook.rs::host_hook_uses_database_only_metadata_and_queues_under_contention)
- Admitted Beads push retains caller Git ownership and caller worktree guards.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::hardening::admitted_beads_push_retains_caller_git_ownership_and_caller_path_guards)
- Worker launch refuses a checkout containing the host-selected resolver.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::hardening::worker_launch_refuses_to_expose_the_selected_resolver_from_the_checkout)
- Worker mount planning rejects host-state ancestors and aliases before staging.
  [test](../crates/wrix-cli/tests/workspace_admission.rs::hardening::worker_mount_planning_refuses_host_state_ancestors_and_aliases_before_staging)

## Out of Scope

- Automatic enrollment, compatibility grouping, migration, repair, merge,
  deletion, rebinding or native-data relocation.
- A repository database as authority, or a repository database requirement.
- Changing deployment/controller ownership or native database formats.
- A lease fencing arbitrary older same-user writers, or atomicity across all
  filesystem observations and external effects.
- Host configuration activation or external-manager lifecycle management.
