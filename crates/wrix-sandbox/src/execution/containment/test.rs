//! Pure native-projection boundary cases, not simulated runtime outcomes.
use super::*;
fn mount(source: &str, destination: &str, writable: bool) -> ProjectionMount {
    ProjectionMount {
        source: source.into(),
        destination: destination.into(),
        writable,
    }
}
fn projection(mounts: Vec<ProjectionMount>) -> Projection {
    Projection {
        id: wrix_state::execution::Container::parse(&"1".repeat(64)).unwrap(),
        image: crate::image::Digest::parse(&format!("sha256:{}", "2".repeat(64))).unwrap(),
        mounts,
    }
}
#[test]
fn start_requires_exact_image_and_complete_unshadowed_readonly_delivery() {
    let expected = [
        mount("/private/store", "/nix/store", false),
        mount("/private/client", "/run/wrix/nix-pool", false),
        mount("/private/client/nix.conf", "/etc/nix/nix.conf", false),
    ];
    let valid = || {
        let mut mounts = expected.to_vec();
        mounts.push(mount("/checkout", "/workspace", true));
        projection(mounts)
    };
    let image = valid().image;
    assert!(verify(&valid(), &image, &expected).is_ok());
    assert!(verify(&valid(), &image, &[]).is_err());
    let mut wrong_image = valid();
    wrong_image.image = crate::image::Digest::parse(&format!("sha256:{}", "3".repeat(64))).unwrap();
    assert!(verify(&wrong_image, &image, &expected).is_err());
    for index in 0..expected.len() {
        let mut missing = valid();
        missing.mounts.remove(index);
        assert!(verify(&missing, &image, &expected).is_err());
        let mut writable = valid();
        writable.mounts[index].writable = true;
        assert!(verify(&writable, &image, &expected).is_err());
        let mut substituted = valid();
        substituted.mounts[index].source = "/other".into();
        assert!(verify(&substituted, &image, &expected).is_err());
        let mut duplicate = valid();
        duplicate.mounts.push(expected[index].clone());
        assert!(verify(&duplicate, &image, &expected).is_err());
    }
    for destination in [
        "/nix",
        "/nix/store/overlay",
        "/run/wrix",
        "/run/wrix/nix-pool/key",
        "/etc/nix",
        "/etc/nix/nix.conf/child",
    ] {
        let mut shadowed = valid();
        shadowed.mounts.push(mount("/overlay", destination, false));
        assert!(
            verify(&shadowed, &image, &expected).is_err(),
            "{destination}"
        );
    }
    let mut sibling = valid();
    sibling
        .mounts
        .push(mount("/other", "/nix/store-extra", true));
    assert!(verify(&sibling, &image, &expected).is_ok());
    for invalid in [
        mount("relative", "/nix/store", false),
        mount("/private/store", "relative", false),
        mount("/private/../store", "/nix/store", false),
        mount("/private/store", "/nix/../nix/store", false),
        mount("/private/store", "/nix/store", true),
    ] {
        assert!(verify(&valid(), &image, &[invalid]).is_err());
    }
    let mut duplicate = expected.to_vec();
    duplicate.push(expected[0].clone());
    assert!(verify(&valid(), &image, &duplicate).is_err());
}
