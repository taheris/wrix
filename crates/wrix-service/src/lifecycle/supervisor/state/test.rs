use super::*;
use wrix_core::path::Workspace;

fn namespace(root: &Path) -> Namespace {
    Namespace::from(Workspace::from_path(root).unwrap().hash())
}

fn open_after_transient_forks(root: &Path, identity: &Namespace) -> Result<Owner, Error> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        match Owner::open(root, identity.clone()) {
            Err(Error::Io(error))
                if error.kind() == io::ErrorKind::WouldBlock
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            result => return result,
        }
    }
}

#[test]
fn unclean_epoch_is_not_released_by_parent_death_or_an_available_lock() {
    let root = tempfile::tempdir().unwrap();
    let identity = namespace(root.path());
    let mut owner = Owner::open(root.path(), identity.clone()).unwrap();
    owner.begin().unwrap();
    drop(owner);
    assert!(matches!(
        open_after_transient_forks(root.path(), &identity),
        Err(Error::Unclean)
    ));
}

#[test]
fn clean_restart_retains_retirement_instead_of_resurrecting_the_namespace() {
    let root = tempfile::tempdir().unwrap();
    let identity = namespace(root.path());
    let mut owner = Owner::open(root.path(), identity.clone()).unwrap();
    owner.begin().unwrap();
    owner.intend(Intent::Retired).unwrap();
    owner.finish().unwrap();
    drop(owner);
    let reopened = open_after_transient_forks(root.path(), &identity).unwrap();
    assert_eq!(reopened.intent(), Intent::Retired);
}

#[test]
fn another_namespace_cannot_adopt_preserved_component_state() {
    let root = tempfile::tempdir().unwrap();
    let peer = tempfile::tempdir().unwrap();
    let mut owner = Owner::open(root.path(), namespace(root.path())).unwrap();
    owner.begin().unwrap();
    owner.finish().unwrap();
    drop(owner);
    let before = fs::read(root.path().join(RECORD)).unwrap();
    assert!(matches!(
        open_after_transient_forks(root.path(), &namespace(peer.path())),
        Err(Error::Incompatible)
    ));
    assert_eq!(fs::read(root.path().join(RECORD)).unwrap(), before);
}
