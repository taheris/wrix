//! A missing failed-recovery owner can yield to the exact pending update, never a live owner.

use super::{Intent, Phase, document};
use crate::lifecycle::{Result, RuntimeStatus, coordination, owner};
use std::{fs::File, io::Write, path::Path};
use wrix_sandbox::image::Digest;

pub(super) fn check(
    state: &Path,
    intent: &Intent,
    previous: &owner::Selected,
    candidate: &owner::Selected,
    status: RuntimeStatus,
    digest: &Digest,
) -> Result<()> {
    if candidate.runtime != previous.runtime
        || candidate.container != previous.container
        || candidate.port != previous.port
        || (candidate.workspace.is_some() && candidate.workspace != previous.workspace)
    {
        return Err(owner::Error::Conflict.into());
    }
    let recipe = candidate.recipe.as_ref().ok_or(owner::Error::Conflict)?;
    if recipe.binding == intent.candidate && recipe.config_digest == *digest {
        return Ok(());
    }
    if status != RuntimeStatus::Missing {
        return Err(owner::Error::Conflict.into());
    }
    match (&intent.ownership, intent.phase) {
        (None, Phase::Preparing) => Ok(()),
        (Some(ownership), Phase::Preparing)
            if ownership.origin.as_ref() == Some(&recipe.image_id) =>
        {
            Ok(())
        }
        (Some(ownership), Phase::Released | Phase::Starting)
            if ownership.origin.as_ref() == Some(&recipe.image_id) =>
        {
            super::super::checkpoint::verify(
                state,
                &intent.attempt,
                &intent.candidate,
                digest,
                ownership.origin.as_ref(),
            )
        }
        _ => Err(owner::Error::Conflict.into()),
    }
}

pub(super) fn archive(
    guard: &coordination::Guard,
    intent: &Intent,
    candidate: &owner::Selected,
) -> Result<()> {
    let root = guard.root();
    let source = root.join("celler-candidate");
    if owner::Selected::read(&source)?.as_ref() != Some(candidate) {
        return Err(owner::Error::Conflict.into());
    }
    let bytes = document(&source, "celler-owner.json")?.ok_or(owner::Error::Conflict)?;
    let name = format!(
        "recovery-owner-before-update-{}.json",
        intent.attempt.as_str()
    );
    if let Some(retained) = document(root, &name)? {
        if retained != bytes {
            return Err(owner::Error::Conflict.into());
        }
    } else {
        let mut file = tempfile::NamedTempFile::new_in(root)?;
        file.write_all(&bytes)?;
        file.as_file().sync_all()?;
        file.persist_noclobber(root.join(&name))
            .map_err(|error| error.error)?;
    }
    File::open(root)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::lifecycle::{RuntimeKind, owner::Recipe};
    use std::{fs, os::unix::fs::MetadataExt};
    use wrix_core::path::ContainerName;

    fn digest(image: char) -> Digest {
        Digest::parse(&format!("sha256:{}", image.to_string().repeat(64))).unwrap()
    }

    fn owner(image: char, intent: &Intent) -> owner::Selected {
        let mut binding = serde_json::to_value(&intent.candidate).unwrap();
        binding["image"] = format!("localhost/wrix-service:{image}").into();
        owner::Selected {
            runtime: RuntimeKind::Container,
            workspace: None,
            container: ContainerName::from_persisted("wrix-host-service".into()).unwrap(),
            port: std::num::NonZeroU16::new(21232).unwrap(),
            recipe: Some(Recipe {
                binding: serde_json::from_value(binding).unwrap(),
                image_id: digest(image),
                config_digest: digest(image),
            }),
        }
    }

    #[test]
    fn missing_failed_recovery_can_yield_to_a_different_exact_update() {
        let mut intent = super::super::test::prepared();
        intent.phase = Phase::Preparing;
        let previous = owner('a', &intent);
        let pending = owner('b', &intent);
        let digest = digest('c');
        assert!(
            check(
                Path::new("/absent"),
                &intent,
                &previous,
                &pending,
                RuntimeStatus::Missing,
                &digest
            )
            .is_ok()
        );
        assert_ne!(pending.recipe, previous.recipe);
        assert_ne!(pending.recipe.as_ref().unwrap().binding, intent.candidate);
    }

    #[test]
    fn preparing_retry_accepts_captured_candidate_origin_before_checkpoint_publication() {
        let mut intent = super::super::test::prepared();
        intent.phase = Phase::Preparing;
        let previous = owner('a', &intent);
        let pending = owner('b', &intent);
        intent.ownership = Some(super::super::Ownership {
            locks: Vec::new(),
            origin: Some(pending.recipe.as_ref().unwrap().image_id.clone()),
        });
        assert!(
            check(
                Path::new("/absent"),
                &intent,
                &previous,
                &pending,
                RuntimeStatus::Missing,
                &digest('c')
            )
            .is_ok()
        );
    }

    #[test]
    fn preparing_retry_refuses_a_different_captured_origin() {
        let mut intent = super::super::test::prepared();
        intent.phase = Phase::Preparing;
        let previous = owner('a', &intent);
        let pending = owner('b', &intent);
        intent.ownership = Some(super::super::Ownership {
            locks: Vec::new(),
            origin: Some(digest('d')),
        });
        assert!(
            check(
                Path::new("/absent"),
                &intent,
                &previous,
                &pending,
                RuntimeStatus::Missing,
                &digest('c')
            )
            .is_err()
        );
    }

    #[test]
    fn different_recovery_target_never_authorizes_replacing_an_existing_container() {
        let mut intent = super::super::test::prepared();
        intent.phase = Phase::Preparing;
        let previous = owner('a', &intent);
        let pending = owner('b', &intent);
        let digest = digest('c');
        for status in [RuntimeStatus::Running, RuntimeStatus::Stopped] {
            assert!(
                check(
                    Path::new("/absent"),
                    &intent,
                    &previous,
                    &pending,
                    status,
                    &digest
                )
                .is_err()
            );
        }
    }

    #[test]
    fn missing_recovery_requires_matching_physical_coordinates() {
        let mut intent = super::super::test::prepared();
        intent.phase = Phase::Preparing;
        let previous = owner('a', &intent);
        let pending = owner('b', &intent);
        let digest = digest('c');
        let mut variants = vec![pending; 5];
        variants[0].runtime = RuntimeKind::Podman;
        variants[1].container = ContainerName::from_persisted("foreign".into()).unwrap();
        variants[2].port = std::num::NonZeroU16::new(22222).unwrap();
        variants[3].recipe = None;
        variants[4].workspace =
            Some(wrix_core::path::WorkspaceHash::parse(&"a".repeat(64)).unwrap());
        for changed in variants {
            assert!(
                check(
                    Path::new("/absent"),
                    &intent,
                    &previous,
                    &changed,
                    RuntimeStatus::Missing,
                    &digest
                )
                .is_err()
            );
        }
    }

    #[test]
    fn different_recovery_target_refuses_unproven_progressed_update() {
        let mut intent = super::super::test::prepared();
        let previous = owner('a', &intent);
        let pending = owner('b', &intent);
        let digest = digest('c');
        for phase in [
            Phase::Prepared,
            Phase::Draining,
            Phase::Releasing,
            Phase::Released,
            Phase::Starting,
            Phase::Checking,
            Phase::Activated,
        ] {
            intent.phase = phase;
            assert!(
                check(
                    Path::new("/absent"),
                    &intent,
                    &previous,
                    &pending,
                    RuntimeStatus::Missing,
                    &digest
                )
                .is_err()
            );
        }
    }

    #[test]
    fn missing_recovery_retry_requires_the_exact_checkpoint_and_origin() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().canonicalize().unwrap();
        let mut intent = super::super::test::prepared();
        intent.phase = Phase::Starting;
        let previous = owner('a', &intent);
        let pending = owner('b', &intent);
        let digest = digest('c');
        let origin = pending.recipe.as_ref().unwrap().image_id.clone();
        intent.ownership = Some(super::super::Ownership {
            locks: Vec::new(),
            origin: Some(origin.clone()),
        });
        assert!(
            check(
                &state,
                &intent,
                &previous,
                &pending,
                RuntimeStatus::Missing,
                &digest
            )
            .is_err()
        );
        let checkpoint = state
            .join("host-lifecycle/checkpoints")
            .join(intent.attempt.as_str());
        wrix_core::private_directory::prepare_with_parents(&state.join("host-lifecycle")).unwrap();
        wrix_core::private_directory::prepare_with_parents(
            &state.join("host-lifecycle/checkpoints"),
        )
        .unwrap();
        wrix_core::private_directory::prepare_with_parents(&checkpoint).unwrap();
        super::super::publish(
            &checkpoint,
            "checkpoint.json",
            &serde_json::json!({
                "version": 1,
                "target": intent.candidate,
                "digest": digest,
                "source": origin,
            }),
        )
        .unwrap();
        assert!(
            check(
                &state,
                &intent,
                &previous,
                &pending,
                RuntimeStatus::Missing,
                &digest
            )
            .is_ok()
        );
        intent.ownership.as_mut().unwrap().origin = Some(self::digest('a'));
        assert!(
            check(
                &state,
                &intent,
                &previous,
                &pending,
                RuntimeStatus::Missing,
                &digest
            )
            .is_err()
        );
        intent.ownership.as_mut().unwrap().origin = Some(origin);
        assert!(
            check(
                &state,
                &intent,
                &previous,
                &pending,
                RuntimeStatus::Missing,
                &self::digest('d')
            )
            .is_err()
        );
    }

    #[test]
    fn recovery_provenance_is_archived_byte_exactly_before_replacement_and_retryable() {
        let root = tempfile::tempdir().unwrap();
        let guard = coordination::acquire(root.path()).unwrap();
        let intent = super::super::test::prepared();
        let pending = owner('b', &intent);
        let source = root.path().join("celler-candidate");
        fs::create_dir(&source).unwrap();
        fs::set_permissions(&source, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        let candidate_guard = coordination::acquire(&source).unwrap();
        pending.record(&candidate_guard).unwrap();
        let bytes = fs::read(source.join("celler-owner.json")).unwrap();
        archive(&guard, &intent, &pending).unwrap();
        archive(&guard, &intent, &pending).unwrap();
        let path = root.path().join(format!(
            "recovery-owner-before-update-{}.json",
            intent.attempt.as_str()
        ));
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(owner::Selected::read(&source).unwrap(), Some(pending));
        assert!(owner::Selected::read(root.path()).unwrap().is_none());
    }

    #[test]
    fn conflicting_provenance_archive_never_overwrites_either_record() {
        let root = tempfile::tempdir().unwrap();
        let guard = coordination::acquire(root.path()).unwrap();
        let intent = super::super::test::prepared();
        let pending = owner('b', &intent);
        let source = root.path().join("celler-candidate");
        let candidate_guard = coordination::acquire(&source).unwrap();
        pending.record(&candidate_guard).unwrap();
        let bytes = fs::read(source.join("celler-owner.json")).unwrap();
        let name = format!(
            "recovery-owner-before-update-{}.json",
            intent.attempt.as_str()
        );
        super::super::publish(root.path(), &name, &serde_json::json!({"foreign": true})).unwrap();
        let retained = fs::read(root.path().join(&name)).unwrap();
        assert!(archive(&guard, &intent, &pending).is_err());
        assert_eq!(fs::read(root.path().join(name)).unwrap(), retained);
        assert_eq!(fs::read(source.join("celler-owner.json")).unwrap(), bytes);
    }
}
