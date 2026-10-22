use super::*;
use wrix_core::path::WorkspaceHash;

#[test]
fn sandbox_pull_credentials_are_exact_read_only_and_expire_after_one_day() {
    let (_temporary, root, metadata) = directories();
    let authority = Authority::provision(&root, &metadata).unwrap();
    for identity in ["a".repeat(64), "b".repeat(64)] {
        let workspace = WorkspaceHash::parse(&identity).unwrap();
        let credential = authority.pull_credential(&workspace).unwrap();
        let parts: Vec<_> = credential.expose_bearer().split('.').collect();
        assert_eq!(parts.len(), 3);
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        let grants = claims["https://jwt.attic.rs/v1"]["caches"]
            .as_object()
            .unwrap();
        assert_eq!(grants.len(), 2);
        assert_eq!(
            grants[credential.views().common().as_str()],
            serde_json::json!({"r": 1})
        );
        assert_eq!(
            grants[credential.views().private().as_str()],
            serde_json::json!({"r": 1})
        );
        assert_eq!(claims["sub"], "wrix-sandbox-pull");
        assert_eq!(
            claims["exp"].as_u64().unwrap() - claims["iat"].as_u64().unwrap(),
            86400
        );
        assert_eq!(
            credential
                .expires_at()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            claims["exp"]
        );
        assert_eq!(
            credential.views().private(),
            &crate::backend::publication::registration::private_namespace(&workspace).unwrap()
        );
    }
}

#[test]
fn namespace_views_do_not_implicitly_group_nested_clones_or_unrelated_paths() {
    use crate::backend::pull::ReadSet;
    use wrix_core::path::Workspace;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("repository");
    let clone = root.join(".loom/beads/worker");
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(clone.join(".git")).unwrap();
    let repository = Workspace::from_service_path(&root).unwrap();
    let worker = Workspace::from_service_path(&clone).unwrap();
    let original = ReadSet::for_workspace(repository.hash()).unwrap();
    let nested = ReadSet::for_workspace(worker.hash()).unwrap();
    assert_eq!(original.common(), nested.common());
    assert_ne!(original.private(), nested.private());
    for name in ["unrelated", "non-repository"] {
        let path = temporary.path().join(name);
        fs::create_dir(&path).unwrap();
        if name == "unrelated" {
            fs::create_dir(path.join(".git")).unwrap();
        }
        let identity = Workspace::from_service_path(&path).unwrap();
        let views = ReadSet::for_workspace(identity.hash()).unwrap();
        assert_eq!(original.common(), views.common());
        assert_ne!(original.private(), views.private());
        assert_eq!(
            views.private(),
            ReadSet::for_workspace(Workspace::from_service_path(&path).unwrap().hash())
                .unwrap()
                .private()
        );
    }
}

#[test]
fn private_read_identity_is_disjoint_while_common_identity_is_shared() {
    let (_temporary, root, metadata) = directories();
    let authority = Authority::provision(&root, &metadata).unwrap();
    let first = authority
        .pull_credential(&WorkspaceHash::parse(&"a".repeat(64)).unwrap())
        .unwrap();
    let second = authority
        .pull_credential(&WorkspaceHash::parse(&"b".repeat(64)).unwrap())
        .unwrap();
    assert_eq!(first.views().common(), second.views().common());
    assert_ne!(first.views().private(), second.views().private());
    assert_ne!(first.views().common(), first.views().private());
}
