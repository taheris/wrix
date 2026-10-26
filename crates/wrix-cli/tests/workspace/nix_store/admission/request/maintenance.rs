//! Actual host metadata, kernel exclusion and private IPC; no VM readiness is fabricated here.
use super::*;
use std::os::unix::fs::PermissionsExt;
use wrix_service::{
    control::{
        Access, Client, Controller, Namespace, Operation, Policy, Reply, supervision::Package,
    },
    nix_store::lifecycle::{Error, Repair},
};
fn package() -> Package {
    serde_json::from_value(serde_json::json!({"controller":format!("/nix/store/{}-controller", "a".repeat(32)),"runtime":format!("/nix/store/{}-runtime", "b".repeat(32)),"image":format!("/nix/store/{}-image.drv", "c".repeat(32))})).unwrap()
}
#[test]
fn pool_repair_excludes_new_reservations_and_rejects_incomplete_inventory_before_mutation() {
    if let Some(caller) = child() {
        let selection = Selection::inspect(&caller).unwrap();
        let host = wrix_state::host::Host::discover().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let root = host.roots().state().join("control/authority");
        let controller = {
            let _entered = runtime.enter();
            Controller::bind(&root).unwrap()
        };
        let client = Client::open(&root).unwrap();
        let (reply, served) = runtime.block_on(async {
            tokio::join!(
                client.request(Operation::Register {
                    namespace: Namespace::from(selection.context().unwrap().key()),
                    policy: Policy {
                        dolt: Access::Denied,
                        private_cache_read: Access::Denied,
                        common_cache_read: Access::Denied,
                        egress: Access::Denied,
                        compiler: Access::Denied
                    }
                }),
                controller.serve_one()
            )
        });
        assert_eq!(reply.unwrap(), Reply::Applied);
        served.unwrap();
        let repair = Repair::begin(&host, &package()).unwrap();
        assert!(matches!(
            controller.reserve_nix_pool(&selection, System::X86_64Linux),
            Err(wrix_service::control::Error::PoolLifecycle(Error::Busy))
        ));
        assert!(!host.roots().state().join("control/nix").exists());
        repair.complete().unwrap();
        let reserved = controller
            .reserve_nix_pool(&selection, System::X86_64Linux)
            .unwrap();
        drop(reserved);
        assert!(Repair::begin(&host, &package()).is_err());
        assert!(
            !host
                .roots()
                .state()
                .join("nix-pool-maintenance.json")
                .exists()
        );
        controller.close().unwrap();
        return;
    }
    let (fixture, checkout) = setup();
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    start(&fixture, &checkout, "nix_store::admission::request::maintenance::pool_repair_excludes_new_reservations_and_rejects_incomplete_inventory_before_mutation").finish();
}
#[test]
fn pool_maintenance_does_not_repair_existing_authority_permissions() {
    if let Some(caller) = child() {
        let host = wrix_state::host::Host::discover().unwrap();
        fs::set_permissions(host.roots().state(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Repair::begin(&host, &package()).is_err());
        assert_eq!(
            fs::metadata(host.roots().state())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert!(
            !host
                .roots()
                .state()
                .join("nix-pool-maintenance.json")
                .exists()
        );
        fs::set_permissions(host.roots().state(), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(Selection::inspect(&caller).is_ok());
        return;
    }
    let (fixture, checkout) = setup();
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    start(&fixture, &checkout, "nix_store::admission::request::maintenance::pool_maintenance_does_not_repair_existing_authority_permissions").finish();
}

#[test]
fn mismatched_pool_maintenance_intent_is_retained_without_provisioning() {
    if let Some(caller) = child() {
        let selection = Selection::inspect(&caller).unwrap();
        let host = wrix_state::host::Host::discover().unwrap();
        let pool = Admission::acquire(&selection, System::X86_64Linux)
            .unwrap()
            .pool()
            .clone();
        let generation = wrix_state::store::Store::inspect(&host)
            .unwrap()
            .unwrap()
            .generation()
            .unwrap();
        let path = host.roots().state().join("nix-pool-maintenance.json");
        let marker = serde_json::json!({"version":1,"generation":generation.as_bytes(),"deployment":"a".repeat(32),"target":package(),"entries":[{"original":"serving","pool":pool}]});
        fs::write(&path, serde_json::to_vec(&marker).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(Repair::begin(&host, &package()).is_err());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&fs::read(&path).unwrap()).unwrap(),
            marker
        );
        assert!(!host.roots().state().join("control/nix").exists());
        return;
    }
    let (fixture, checkout) = setup();
    deployment(&fixture);
    success(&fixture, &checkout, &["enable"]);
    start(&fixture, &checkout, "nix_store::admission::request::maintenance::mismatched_pool_maintenance_intent_is_retained_without_provisioning").finish();
}
