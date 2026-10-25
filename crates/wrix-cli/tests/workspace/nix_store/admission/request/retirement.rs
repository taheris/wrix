//! Private metadata state controls, not fabricated native runtime acceptance.
use super::*;
use std::sync::Arc;
use wrix_state::{
    execution::{self, Container, Recorded, Removal},
    store::{Generation, Store},
};

#[test]
fn recorded_removal_intent_is_durable_generation_fenced_and_not_terminal() {
    if let Some(caller) = child() {
        let selection = Selection::inspect(&caller).unwrap();
        let host = Arc::new(wrix_state::host::Host::discover().unwrap());
        let context = selection.context().unwrap();
        let generation = Store::inspect(&host)
            .unwrap()
            .unwrap()
            .generation()
            .unwrap();
        let ticket = execution::reserve(&host, context).unwrap();
        assert!(
            Recorded::load(host.clone(), context.key(), &ticket, &generation)
                .unwrap()
                .is_none()
        );
        let bound = Admission::acquire(&selection, System::X86_64Linux)
            .unwrap()
            .handoff(ticket.clone())
            .unwrap()
            .bind(Container::parse(&"1".repeat(64)).unwrap())
            .unwrap();
        let recorded = bound.recorded().unwrap();
        assert!(recorded.retire().is_err());
        assert!(Generation::parse(&[0; 31]).is_err());
        let wrong = Generation::parse(if generation.as_bytes() == &[0; 32] {
            &[1; 32]
        } else {
            &[0; 32]
        })
        .unwrap();
        assert!(Recorded::load(host.clone(), context.key(), &ticket, &wrong).is_err());
        recorded.removing().unwrap();
        recorded.removing().unwrap();
        assert_eq!(recorded.status().unwrap(), Removal::Removing);
        assert!(
            execution::terminal(&host, context.key(), &ticket)
                .unwrap()
                .is_none()
        );
        drop(recorded);
        let recovered = Recorded::load(host.clone(), context.key(), &ticket, &generation)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.status().unwrap(), Removal::Removing);
        recovered.retire().unwrap();
        recovered.retire().unwrap();
        assert_eq!(recovered.status().unwrap(), Removal::Retired);
        assert!(
            execution::terminal(&host, context.key(), &ticket)
                .unwrap()
                .is_some()
        );
        assert!(
            Admission::acquire(&selection, System::X86_64Linux)
                .unwrap()
                .handoff(ticket)
                .is_err()
        );
        return;
    }
    let (fixture, checkout) = setup();
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    start(&fixture, &checkout, "nix_store::admission::request::retirement::recorded_removal_intent_is_durable_generation_fenced_and_not_terminal").finish();
}

#[test]
fn existing_native_observation_does_not_rediscover_changed_admission_scope() {
    if let Some(caller) = child() {
        let selection = Selection::inspect(&caller).unwrap();
        let host = wrix_state::host::Host::discover().unwrap();
        let context = selection.context().unwrap();
        let ticket = execution::reserve(&host, context).unwrap();
        let bound = Admission::acquire(&selection, System::X86_64Linux)
            .unwrap()
            .handoff(ticket.clone())
            .unwrap()
            .bind(Container::parse(&"1".repeat(64)).unwrap())
            .unwrap();
        ready_and_wait();
        assert!(bound.recheck().is_err());
        assert!(Admission::acquire(&selection, System::X86_64Linux).is_err());
        let recorded = bound.recorded().unwrap();
        assert_eq!(recorded.status().unwrap(), Removal::Bound);
        recorded.removing().unwrap();
        recorded.retire().unwrap();
        assert!(
            execution::terminal(&host, context.key(), &ticket)
                .unwrap()
                .is_some()
        );
        return;
    }
    let (fixture, checkout) = setup();
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    let issuer = start(
        &fixture,
        &checkout,
        "nix_store::admission::request::retirement::existing_native_observation_does_not_rediscover_changed_admission_scope",
    );
    wait_for(&fixture.root.path().join("ready"));
    let repository = fixture.root.path().join("checkout");
    fs::rename(repository.join(".git"), repository.join(".git-retained")).unwrap();
    assert!(fixture.git(&checkout, &["init", "-q"]).status.success());
    fs::write(fixture.root.path().join("release"), b"release").unwrap();
    issuer.finish();
}
