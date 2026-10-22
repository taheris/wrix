use std::path::Path;
use wrix_core::nix_store::pool::{Key, Repository, System};
use wrix_service::deployment::Layout;

#[test]
fn pool_layout_separates_repositories_systems_and_private_control_without_effects() {
    let root = tempfile::tempdir().unwrap();
    let layout = Layout::new(root.path(), Path::new("/cache")).unwrap();
    let x86 = Key {
        repository: Repository::try_from("a".repeat(32)).unwrap(),
        system: System::X86_64Linux,
    };
    let arm = Key {
        repository: x86.repository.clone(),
        system: System::Aarch64Linux,
    };
    let peer = Key {
        repository: Repository::try_from("b".repeat(32)).unwrap(),
        system: x86.system,
    };
    let paths = [
        layout.nix_pool(&x86),
        layout.nix_pool(&arm),
        layout.nix_pool(&peer),
    ];
    assert_ne!(paths[0], paths[1]);
    assert_ne!(paths[0], paths[2]);
    for path in &paths {
        assert!(path.starts_with(root.path().join("wrix/service-data/nix")));
        assert!(!path.starts_with(layout.control_root()));
        assert!(!path.exists());
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}
