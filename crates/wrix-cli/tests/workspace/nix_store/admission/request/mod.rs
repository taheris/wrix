//! Protected, one-use transport of the original checked context, not a namespace/path surrogate.
mod handoff;
mod native;
mod worker;

use super::*;
use wrix_state::nix_store::admission::request;

#[test]
fn execution_requests_require_an_original_unstarted_native_reservation() {
    if let Some(caller) = child() {
        let selection = Selection::inspect(&caller).unwrap();
        let context = selection.context().unwrap();
        let host = wrix_state::host::Host::discover().unwrap();
        let forged = wrix_state::execution::Ticket::parse(&"a".repeat(64)).unwrap();
        assert!(
            Admission::acquire(&selection, System::X86_64Linux)
                .unwrap()
                .request_execution(forged)
                .is_err()
        );
        let ticket = wrix_state::execution::reserve(&host, context).unwrap();
        let request = Admission::acquire(&selection, System::X86_64Linux)
            .unwrap()
            .request_execution(ticket.clone())
            .unwrap();
        let accepted = request::claim(request.id()).unwrap();
        assert!(
            matches!(accepted.purpose(), request::Purpose::Execution { ticket: bound } if bound == &ticket)
        );
        wrix_state::execution::begin(&host, context, &ticket).unwrap();
        assert!(wrix_state::execution::cancel(&host, context, &ticket).is_err());
        assert!(
            wrix_state::execution::terminal(&host, context.key(), &ticket)
                .unwrap()
                .is_none()
        );
        assert!(
            Admission::acquire(&selection, System::X86_64Linux)
                .unwrap()
                .request_execution(ticket)
                .is_err()
        );
        let cancelled = wrix_state::execution::reserve(&host, context).unwrap();
        wrix_state::execution::cancel(&host, context, &cancelled).unwrap();
        assert!(wrix_state::execution::begin(&host, context, &cancelled).is_err());
        assert!(
            wrix_state::execution::terminal(&host, context.key(), &cancelled)
                .unwrap()
                .is_some()
        );
        assert!(
            Admission::acquire(&selection, System::X86_64Linux)
                .unwrap()
                .request_execution(cancelled)
                .is_err()
        );
        let peer = Selection::inspect(&caller.parent().unwrap().join("peer")).unwrap();
        let foreign = wrix_state::execution::reserve(&host, peer.context().unwrap()).unwrap();
        assert!(
            Admission::acquire(&selection, System::X86_64Linux)
                .unwrap()
                .request_execution(foreign)
                .is_err()
        );
        assert!(!host.roots().state().join("control/nix").exists());
        return;
    }
    let (fixture, checkout) = setup();
    fixture.repository("peer");
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    start(&fixture, &checkout, "nix_store::admission::request::execution_requests_require_an_original_unstarted_native_reservation").finish();
}

#[test]
fn checked_preparation_requests_release_approval_exclusion_and_cannot_be_replayed() {
    if let Some(caller) = child() {
        let selection = Selection::inspect(&caller).unwrap();
        let guard = Admission::acquire(&selection, System::X86_64Linux).unwrap();
        let pool = guard.pool().clone();
        let request = guard.request().unwrap();
        let host = wrix_state::host::Host::discover().unwrap();
        let lock = fs::File::open(host.roots().state().join("state.lock")).unwrap();
        lock.try_lock_exclusive().unwrap();
        drop(lock);
        let accepted = request::claim(request.id()).unwrap();
        assert_eq!(accepted.pool(), &pool);
        assert_eq!(accepted.namespace(), selection.context().unwrap().key());
        accepted.recheck().unwrap();
        assert!(request::claim(request.id()).is_err());
        assert!(!host.roots().state().join("control/nix").exists());
        return;
    }
    let (fixture, checkout) = setup();
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    start(&fixture, &checkout, "nix_store::admission::request::checked_preparation_requests_release_approval_exclusion_and_cannot_be_replayed").finish();
}

#[test]
fn disabled_approval_rejects_a_previously_published_preparation_request_before_effects() {
    if let Some(caller) = child() {
        let selection = Selection::inspect(&caller).unwrap();
        let request = Admission::acquire(&selection, System::X86_64Linux)
            .unwrap()
            .request()
            .unwrap();
        ready_and_wait();
        assert!(matches!(
            request::claim(request.id()),
            Err(wrix_state::nix_store::Error::ApprovalRequired)
        ));
        return;
    }
    let (fixture, checkout) = setup();
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    let issuer = start(
        &fixture,
        &checkout,
        "nix_store::admission::request::disabled_approval_rejects_a_previously_published_preparation_request_before_effects",
    );
    wait_for(&fixture.root.path().join("ready"));
    success(&fixture, &checkout, &["disable"]);
    fs::write(fixture.root.path().join("release"), b"release").unwrap();
    issuer.finish();
    assert!(!fixture.state().join("control/nix").exists());
}

#[test]
fn preparation_claim_rechecks_the_frozen_original_integrated_primary() {
    if let Some(caller) = child() {
        let selection = Selection::inspect(&caller).unwrap();
        assert!(selection.context().unwrap().integrated());
        let request = Admission::acquire(&selection, System::X86_64Linux)
            .unwrap()
            .request()
            .unwrap();
        ready_and_wait();
        assert!(request::claim(request.id()).is_err());
        return;
    }
    let (fixture, checkout) = setup();
    let primary = fixture.repository("primary");
    let replacement = fixture.repository("replacement");
    fixture.adapter(&checkout, &primary, "pool-member");
    assert!(
        fixture
            .admit(&checkout, "pool-member", &["--allocate"])
            .status
            .success()
    );
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    let issuer = start(
        &fixture,
        &checkout,
        "nix_store::admission::request::preparation_claim_rechecks_the_frozen_original_integrated_primary",
    );
    wait_for(&fixture.root.path().join("ready"));
    let response = serde_json::json!({"version":1,"membership":"pool-member","checkout_root_b64":wrix_state::resolver::encode(&checkout),"primary_root_b64":wrix_state::resolver::encode(&replacement)});
    let bash = fixture.install("bash");
    fs::write(fixture.root.path().join("adapter"), format!("#!{}\nset -euo pipefail\nIFS= read -r request || [[ -n \"$request\" ]]\nprintf '%s' '{}'\n", bash.display(), response.to_string().replace('\'', "'\\''"))).unwrap();
    fs::write(fixture.root.path().join("release"), b"release").unwrap();
    issuer.finish();
    assert!(!fixture.state().join("control/nix").exists());
}
