//! Host metadata controls only; no fabricated runtime or filesystem acceptance.
use super::*;
use wrix_sandbox::nix_store::{Attempt, Delivery, Unavailable};
use wrix_state::execution::{self, Phase};
fn binding() -> wrix_core::nix_store::binding::Binding {
    wrix_core::nix_store::binding::Binding::new(&format!("sha256:{}", "a".repeat(64)), serde_json::from_value(serde_json::json!({"version":1,"seed_closure":"/nix/store/00000000000000000000000000000000-closure-info/store-paths"})).unwrap()).unwrap()
}
#[test]
fn failed_factory_cancels_only_the_original_never_handed_off_reservation() {
    if let Some(caller) = child() {
        let selection = Selection::inspect(&caller).unwrap();
        let host = wrix_state::host::Host::discover().unwrap();
        let context = selection.context().unwrap();
        let attempt = Attempt::reserve(&selection, System::X86_64Linux, binding(), None).unwrap();
        let ticket = attempt.ticket().clone();
        assert!(attempt.prepare(&Unavailable).is_err());
        assert_eq!(
            execution::observe(&host, context, &ticket).unwrap(),
            Some(Phase::Cancelled)
        );
        assert!(
            execution::terminal(&host, context.key(), &ticket)
                .unwrap()
                .is_some()
        );
        let ticket = execution::reserve(&host, context).unwrap();
        let attempt =
            Attempt::reserve(&selection, System::X86_64Linux, binding(), Some(&ticket)).unwrap();
        execution::begin(&host, context, &ticket).unwrap();
        drop(attempt);
        assert_eq!(
            execution::observe(&host, context, &ticket).unwrap(),
            Some(Phase::Launching)
        );
        assert!(
            execution::terminal(&host, context.key(), &ticket)
                .unwrap()
                .is_none()
        );
        assert!(!host.roots().state().join("control/nix").exists());
        return;
    }
    let (fixture, checkout) = setup();
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    start(&fixture, &checkout, "nix_store::admission::request::factory::failed_factory_cancels_only_the_original_never_handed_off_reservation").finish();
}
#[test]
fn factory_reservation_rejects_a_foreign_namespace_without_cancelling_its_work() {
    if let Some(caller) = child() {
        let selection = Selection::inspect(&caller).unwrap();
        let peer = Selection::inspect(&caller.parent().unwrap().join("peer")).unwrap();
        let host = wrix_state::host::Host::discover().unwrap();
        let ticket = execution::reserve(&host, peer.context().unwrap()).unwrap();
        assert!(
            Attempt::reserve(&selection, System::X86_64Linux, binding(), Some(&ticket)).is_err()
        );
        assert_eq!(
            execution::observe(&host, peer.context().unwrap(), &ticket).unwrap(),
            Some(Phase::Reserved)
        );
        return;
    }
    let (fixture, checkout) = setup();
    fixture.repository("peer");
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    start(&fixture, &checkout, "nix_store::admission::request::factory::factory_reservation_rejects_a_foreign_namespace_without_cancelling_its_work").finish();
}
#[test]
fn factory_delivery_derives_only_fixed_execution_private_sources_and_exact_endpoint() {
    if let Some(caller) = child() {
        let selection = Selection::inspect(&caller).unwrap();
        let host = wrix_state::host::Host::discover().unwrap();
        let ticket = execution::reserve(&host, selection.context().unwrap()).unwrap();
        let pool = Admission::acquire(&selection, System::X86_64Linux)
            .unwrap()
            .pool()
            .clone();
        let expected = host
            .roots()
            .state()
            .join("control/nix")
            .join(pool.repository.to_string())
            .join(pool.system.as_str())
            .join("executions")
            .join(ticket.as_str());
        let delivery =
            Delivery::podman(pool, ticket, std::num::NonZeroU16::new(12345).unwrap()).unwrap();
        let mounts = delivery.mounts();
        assert_eq!(mounts[0].source, expected.join("store"));
        assert_eq!(mounts[1].source, expected.join("worker"));
        assert_eq!(mounts[2].source, expected.join("worker/nix.conf"));
        assert!(mounts.iter().all(|mount| !mount.writable));
        let mut command = std::process::Command::new("podman");
        delivery.configure(&mut command);
        let args: Vec<_> = command
            .get_args()
            .map(std::ffi::OsStr::to_os_string)
            .collect();
        assert_eq!(args.len(), 8);
        assert_eq!(args[0], "-v");
        assert_eq!(args[2], "-v");
        assert_eq!(args[4], "-v");
        for index in [1, 3, 5] {
            assert!(args[index].to_str().unwrap().ends_with(":ro"));
        }
        assert_eq!(args[6], "-e");
        assert_eq!(
            args[7],
            "WRIX_NETWORK_LOCAL_ENDPOINTS=169.254.1.2:12345/tcp"
        );
        assert!(!expected.exists());
        return;
    }
    let (fixture, checkout) = setup();
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    start(&fixture, &checkout, "nix_store::admission::request::factory::factory_delivery_derives_only_fixed_execution_private_sources_and_exact_endpoint").finish();
}
