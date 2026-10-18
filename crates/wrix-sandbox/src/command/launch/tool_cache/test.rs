use super::*;

#[test]
fn private_data_is_writable_fresh_and_disposed_without_touching_other_data()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let sentinel = root.path().join("retained-worker-data");
    fs::write(&sentinel, "retained")?;
    for index in 0..2 {
        let staging = Staging {
            root: root.path().join(format!("launch-{index}")),
        };
        fs::create_dir(&staging.root)?;
        let mounts = prepare(
            &[ManagedCache::Cargo, ManagedCache::Sccache, ManagedCache::Uv],
            Mode::Private,
            &staging,
        )?;
        assert_eq!(mounts.len(), 3);
        for mount in mounts {
            let host = Path::new(&mount.host);
            assert!(!host.join("written").exists());
            fs::write(host.join("written"), "private")?;
            assert_eq!(fs::read_to_string(host.join("written"))?, "private");
        }
        fs::remove_dir_all(&staging.root)?;
        assert_eq!(fs::read_to_string(&sentinel)?, "retained");
    }
    Ok(())
}
