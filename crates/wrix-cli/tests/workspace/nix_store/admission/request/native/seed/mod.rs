//! Factory-built worker images, private-controller seeding and no-transfer installed reuse.
use super::*;
use wrix_sandbox::image::{self, Digest, ImageRef, InstallRequest, Runtime, Source, SourceKind};
use wrix_service::nix_store::owner::backend::{
    Owner,
    seed::{Image, Outcome},
};
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    source: PathBuf,
    source_kind: SourceKind,
    digest_file: PathBuf,
    shared: wrix_core::nix_store::Image,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    primary: Case,
    next: Case,
}
fn fixture() -> Fixture {
    serde_json::from_slice(
        &fs::read(std::env::var_os("WRIX_TEST_POOL_WORKER_IMAGE").unwrap()).unwrap(),
    )
    .unwrap()
}
fn install(case: &Case, name: &str) -> (Digest, wrix_core::nix_store::binding::Binding) {
    let digest = Digest::parse(fs::read_to_string(&case.digest_file).unwrap().trim()).unwrap();
    let reference = ImageRef::parse(name).unwrap();
    let source = Source::parse(&case.source, case.source_kind).unwrap();
    image::install(
        &mut image::CommandStore,
        &InstallRequest::new(Runtime::Podman, &reference, &source, Some(&digest)).unwrap(),
    )
    .unwrap();
    let binding =
        wrix_core::nix_store::binding::Binding::new(digest.as_str(), case.shared.clone()).unwrap();
    (digest, binding)
}
pub(super) fn binding() -> wrix_core::nix_store::binding::Binding {
    install(&fixture().primary, "wrix-pool-worker-seed:latest").1
}

pub(super) fn exercise(
    owner: &Owner,
    issued: &wrix_state::nix_store::admission::request::Accepted,
    delivery: &wrix_service::nix_store::owner::backend::client::Delivery,
) {
    let fixture = fixture();
    let (digest, _) = install(&fixture.primary, "wrix-pool-worker-seed:latest");
    let image = Image::inspect(system(), &digest, &fixture.primary.shared).unwrap();
    assert_eq!(
        owner.seed_execution(issued, &image).unwrap(),
        Outcome::Reused
    );
    for path in image.closure_paths() {
        assert!(
            fs::symlink_metadata(delivery.store().join(path.basename())).is_ok(),
            "seeded image path missing: {}",
            path.as_str()
        );
    }
    let (digest, _) = install(&fixture.next, "wrix-pool-worker-seed-next:latest");
    let next = Image::inspect(system(), &digest, &fixture.next.shared).unwrap();
    assert_ne!(image.id(), next.id());
    assert_eq!(image.closure_paths(), next.closure_paths());
    assert_eq!(
        owner.seed_execution(issued, &next).unwrap(),
        Outcome::Linked
    );
    assert_eq!(
        owner.seed_execution(issued, &next).unwrap(),
        Outcome::Reused
    );
    let host = wrix_state::host::Host::discover().unwrap();
    for image in [&image, &next] {
        assert!(
            !host
                .roots()
                .state()
                .join("control/nix")
                .join(owner.pool().repository.to_string())
                .join(owner.pool().system.as_str())
                .join("seed-roots")
                .join(image.id().as_str())
                .exists()
        );
    }
}

pub(super) fn retained(owner: &Owner, selection: &Selection, host: &wrix_state::host::Host) {
    let ticket = wrix_state::execution::reserve(host, selection.context().unwrap()).unwrap();
    let request = Admission::acquire(selection, system())
        .unwrap()
        .request_execution(ticket.clone())
        .unwrap();
    let issued = request::claim(request.id()).unwrap();
    let fixture = fixture();
    for (case, name) in [
        (&fixture.primary, "wrix-pool-worker-seed:latest"),
        (&fixture.next, "wrix-pool-worker-seed-next:latest"),
    ] {
        let (digest, _) = install(case, name);
        let image = Image::inspect(system(), &digest, &case.shared).unwrap();
        assert_eq!(
            owner.seed_execution(&issued, &image).unwrap(),
            Outcome::Reused
        );
    }
    wrix_state::execution::cancel(host, selection.context().unwrap(), &ticket).unwrap();
}
