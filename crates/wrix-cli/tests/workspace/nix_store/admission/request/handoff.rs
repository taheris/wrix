//! Metadata handoff controls only; synthetic IDs here are not native runtime acceptance.
use super::*;
use wrix_state::execution::{self, Container, Phase, Ticket};

#[test]
fn approved_handoff_consumes_only_original_reserved_tickets_and_releases_the_writer() {
    if let Some(caller) = child() {
        let selection = Selection::inspect(&caller).unwrap();
        let context = selection.context().unwrap();
        let host = wrix_state::host::Host::discover().unwrap();
        let ticket = execution::reserve(&host, context).unwrap();
        let cancelled = execution::reserve(&host, context).unwrap();
        execution::cancel(&host, context, &cancelled).unwrap();
        let peer = Selection::inspect(&caller.parent().unwrap().join("peer")).unwrap();
        let foreign = execution::reserve(&host, peer.context().unwrap()).unwrap();
        for invalid in [
            Ticket::parse(&"f".repeat(64)).unwrap(),
            cancelled,
            foreign.clone(),
        ] {
            assert!(
                Admission::acquire(&selection, System::X86_64Linux)
                    .unwrap()
                    .handoff(invalid)
                    .is_err()
            );
        }
        assert_eq!(
            execution::observe(&host, peer.context().unwrap(), &foreign).unwrap(),
            Some(Phase::Reserved)
        );
        let handoff = Admission::acquire(&selection, System::X86_64Linux)
            .unwrap()
            .handoff(ticket.clone())
            .unwrap();
        let lock = fs::File::open(host.roots().state().join("state.lock")).unwrap();
        lock.try_lock_exclusive().unwrap();
        drop(lock);
        assert_eq!(
            execution::observe(&host, context, &ticket).unwrap(),
            Some(Phase::Launching)
        );
        assert!(execution::cancel(&host, context, &ticket).is_err());
        assert!(
            Admission::acquire(&selection, System::X86_64Linux)
                .unwrap()
                .handoff(ticket.clone())
                .is_err()
        );
        let bound = handoff
            .bind(Container::parse(&"1".repeat(64)).unwrap())
            .unwrap();
        assert_eq!(
            bound.observe().unwrap(),
            Phase::Bound(bound.container().clone())
        );
        drop(bound);
        assert!(
            execution::terminal(&host, context.key(), &ticket)
                .unwrap()
                .is_none()
        );
        let abandoned = execution::reserve(&host, context).unwrap();
        drop(
            Admission::acquire(&selection, System::X86_64Linux)
                .unwrap()
                .handoff(abandoned.clone())
                .unwrap(),
        );
        assert_eq!(
            execution::observe(&host, context, &abandoned).unwrap(),
            Some(Phase::Launching)
        );
        assert!(
            execution::terminal(&host, context.key(), &abandoned)
                .unwrap()
                .is_none()
        );
        return;
    }
    let (fixture, checkout) = setup();
    fixture.repository("peer");
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    start(&fixture, &checkout, "nix_store::admission::request::handoff::approved_handoff_consumes_only_original_reserved_tickets_and_releases_the_writer").finish();
}

#[test]
fn approved_handoff_binding_rejects_repository_substitution_instead_of_rediscovery() {
    if let Some(caller) = child() {
        let selection = Selection::inspect(&caller).unwrap();
        let context = selection.context().unwrap();
        let host = wrix_state::host::Host::discover().unwrap();
        let ticket = execution::reserve(&host, context).unwrap();
        let handoff = Admission::acquire(&selection, System::X86_64Linux)
            .unwrap()
            .handoff(ticket.clone())
            .unwrap();
        ready_and_wait();
        assert!(
            handoff
                .bind(Container::parse(&"1".repeat(64)).unwrap())
                .is_err()
        );
        assert!(
            execution::terminal(&host, context.key(), &ticket)
                .unwrap()
                .is_none()
        );
        return;
    }
    let (fixture, checkout) = setup();
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    let issuer = start(
        &fixture,
        &checkout,
        "nix_store::admission::request::handoff::approved_handoff_binding_rejects_repository_substitution_instead_of_rediscovery",
    );
    wait_for(&fixture.root.path().join("ready"));
    let repository = fixture.root.path().join("checkout");
    fs::rename(repository.join(".git"), repository.join(".git-retained")).unwrap();
    assert!(fixture.git(&checkout, &["init", "-q"]).status.success());
    fs::write(fixture.root.path().join("release"), b"release").unwrap();
    issuer.finish();
}

#[test]
fn approval_withdrawal_after_committed_handoff_does_not_recall_admitted_work() {
    if let Some(caller) = child() {
        let selection = Selection::inspect(&caller).unwrap();
        let context = selection.context().unwrap();
        let host = wrix_state::host::Host::discover().unwrap();
        let ticket = execution::reserve(&host, context).unwrap();
        let handoff = Admission::acquire(&selection, System::X86_64Linux)
            .unwrap()
            .handoff(ticket.clone())
            .unwrap();
        ready_and_wait();
        assert!(Admission::acquire(&selection, System::X86_64Linux).is_err());
        let bound = handoff
            .bind(Container::parse(&"1".repeat(64)).unwrap())
            .unwrap();
        assert_eq!(
            bound.observe().unwrap(),
            Phase::Bound(bound.container().clone())
        );
        assert!(execution::cancel(&host, context, &ticket).is_err());
        return;
    }
    let (fixture, checkout) = setup();
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    let issuer = start(
        &fixture,
        &checkout,
        "nix_store::admission::request::handoff::approval_withdrawal_after_committed_handoff_does_not_recall_admitted_work",
    );
    wait_for(&fixture.root.path().join("ready"));
    success(&fixture, &checkout, &["disable"]);
    fs::write(fixture.root.path().join("release"), b"release").unwrap();
    issuer.finish();
}
