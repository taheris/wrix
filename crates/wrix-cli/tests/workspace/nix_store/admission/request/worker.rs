use super::*;
#[test]
fn worker_intent_freezes_exact_image_and_requires_supported_metadata_and_original_reserved_ticket()
{
    if let Some(caller) = child() {
        let selection = Selection::inspect(&caller).unwrap();
        let context = selection.context().unwrap();
        let host = wrix_state::host::Host::discover().unwrap();
        let ticket = wrix_state::execution::reserve(&host, context).unwrap();
        let metadata: wrix_core::nix_store::Image = serde_json::from_value(serde_json::json!({"version":1,"seed_closure":"/nix/store/00000000000000000000000000000000-closure-info/store-paths"})).unwrap();
        let digest = format!("sha256:{}", "a".repeat(64));
        let image = wrix_core::nix_store::binding::Binding::new(&digest, metadata.clone()).unwrap();
        let mut unsupported = metadata;
        unsupported.version = serde_json::from_value(serde_json::json!(2)).unwrap();
        let unsupported =
            wrix_core::nix_store::binding::Binding::new(&digest, unsupported).unwrap();
        assert!(matches!(
            Admission::acquire(&selection, System::X86_64Linux)
                .unwrap()
                .request_worker(ticket.clone(), unsupported),
            Err(wrix_state::nix_store::Error::Unsupported)
        ));
        let forged = wrix_state::execution::Ticket::parse(&"b".repeat(64)).unwrap();
        assert!(
            Admission::acquire(&selection, System::X86_64Linux)
                .unwrap()
                .request_worker(forged, image.clone())
                .is_err()
        );
        let request = Admission::acquire(&selection, System::X86_64Linux)
            .unwrap()
            .request_worker(ticket.clone(), image.clone())
            .unwrap();
        let accepted = request::claim(request.id()).unwrap();
        match accepted.purpose() {
            request::Purpose::Worker {
                ticket: saved,
                image: saved_image,
            } => {
                assert_eq!(saved, &ticket);
                assert_eq!(saved_image, &image);
            }
            _ => panic!("lost exact worker purpose"),
        }
        accepted.recheck().unwrap();
        wrix_state::execution::cancel(&host, context, &ticket).unwrap();
        assert!(accepted.recheck().is_err());
        assert!(
            Admission::acquire(&selection, System::X86_64Linux)
                .unwrap()
                .request_worker(ticket, image)
                .is_err()
        );
        assert!(!host.roots().state().join("control/nix").exists());
        return;
    }
    let (fixture, checkout) = setup();
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    start(&fixture, &checkout, "nix_store::admission::request::worker::worker_intent_freezes_exact_image_and_requires_supported_metadata_and_original_reserved_ticket").finish();
}
