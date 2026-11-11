use super::*;
use serde_json::json;
use std::fs;

const ROOT: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-root";
const LINK: &str = "nix/var/nix/gcroots/wrix-image/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-root";
const DIRECTORIES: &[(&str, u64, u32)] = &[
    ("nix", 0, 0o755),
    ("nix/var", 0, 0o755),
    ("nix/var/nix", 0, 0o755),
    ("nix/store", 1000, 0o755),
    ("nix/var/nix/db", 1000, 0o700),
    ("nix/var/nix/temproots", 1000, 0o700),
    ("nix/var/nix/builds", 1000, 0o755),
    ("nix/var/nix/profiles", 0, 0o755),
    ("nix/var/nix/profiles/per-user", 0, 0o755),
    ("nix/var/nix/profiles/per-user/wrix", 1000, 0o755),
    ("nix/var/nix/gcroots", 0, 0o755),
    ("nix/var/nix/gcroots/per-user", 0, 0o755),
    ("nix/var/nix/gcroots/wrix-image", 0, 0o755),
    ("nix/var/nix/gcroots/per-user/wrix", 1000, 0o755),
    ("nix/var/nix/gcroots/auto", 0, 0o1777),
    ("nix/var/log/nix/drvs", 1000, 0o755),
];

#[derive(Clone)]
struct Member {
    name: String,
    uid: u64,
    gid: u64,
    mode: u32,
    kind: tar::EntryType,
    bytes: Vec<u8>,
    link: Option<String>,
}

impl Member {
    fn file(name: &str, bytes: &[u8]) -> Self {
        Self {
            name: name.to_owned(),
            uid: 0,
            gid: 0,
            mode: 0o644,
            kind: tar::EntryType::Regular,
            bytes: bytes.to_vec(),
            link: None,
        }
    }
}

fn members() -> Vec<Member> {
    let mut entries: Vec<_> = DIRECTORIES
        .iter()
        .map(|&(name, uid, mode)| Member {
            name: name.to_owned(),
            uid,
            gid: uid,
            mode,
            kind: tar::EntryType::Directory,
            bytes: vec![],
            link: None,
        })
        .collect();
    entries.push(Member {
        uid: 1000,
        gid: 1000,
        mode: 0o600,
        ..Member::file("nix/var/nix/gc.lock", b"")
    });
    entries.push(Member::file("nix/var/nix/db/db.sqlite", b"retained bytes"));
    entries.push(Member::file("etc/wrix/private-nix-version", b"1\n"));
    entries.push(Member::file(
        "etc/wrix/store-roots",
        format!("{ROOT}\n").as_bytes(),
    ));
    entries.push(Member {
        kind: tar::EntryType::Symlink,
        link: Some(ROOT.to_owned()),
        ..Member::file(LINK, b"")
    });
    entries
}

fn layer(entries: &[Member]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for entry in entries {
        let mut header = tar::Header::new_gnu();
        header.set_uid(entry.uid);
        header.set_gid(entry.gid);
        header.set_mode(entry.mode);
        header.set_entry_type(entry.kind);
        header.set_size(entry.bytes.len() as u64);
        if let Some(link) = &entry.link {
            header.set_link_name(link).unwrap();
        }
        header.set_cksum();
        builder
            .append_data(&mut header, &entry.name, entry.bytes.as_slice())
            .unwrap();
    }
    builder.into_inner().unwrap()
}

fn config() -> Value {
    json!({"config":{"Labels":{"wrix.nix.private":"1"},"Env":["IS_SANDBOX=1"]}})
}

fn fixture(entries: &[Member], config: &Value, lower: bool) -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().unwrap();
    let mut layers = vec!["final.tar"];
    let mut outer = vec![
        Member::file("config.json", &serde_json::to_vec(config).unwrap()),
        Member::file("final.tar", &layer(entries)),
    ];
    if lower {
        outer.push(Member::file("lower.tar", &layer(&members())));
        layers.insert(0, "lower.tar");
    }
    outer.push(Member::file(
        "manifest.json",
        &serde_json::to_vec(&json!([{"Config":"config.json","Layers":layers}])).unwrap(),
    ));
    fs::write(file.path(), layer(&outer)).unwrap();
    file
}

fn rejected(entries: &[Member], config: &Value) {
    let file = fixture(entries, config, false);
    assert!(matches!(
        run(file.path(), Format::Archive),
        Err(Error::Contract(_))
    ));
}

#[test]
fn archive_header_metadata_and_retained_private_state_are_read_from_bytes() {
    let file = fixture(&members(), &config(), false);
    run(file.path(), Format::Archive).unwrap();
}

#[test]
fn every_required_owner_group_and_mode_is_checked() {
    for name in DIRECTORIES
        .iter()
        .map(|entry| entry.0)
        .chain(["nix/var/nix/gc.lock"])
    {
        for field in 0..3 {
            let mut entries = members();
            let entry = entries.iter_mut().find(|entry| entry.name == name).unwrap();
            match field {
                0 => entry.uid += 1,
                1 => entry.gid += 1,
                _ => entry.mode ^= 1,
            }
            rejected(&entries, &config());
        }
    }
}

#[test]
fn required_types_and_presence_are_not_inferred_from_names() {
    for name in DIRECTORIES
        .iter()
        .map(|entry| entry.0)
        .chain(["nix/var/nix/gc.lock"])
    {
        let mut entries = members();
        let entry = entries.iter_mut().find(|entry| entry.name == name).unwrap();
        entry.kind = if entry.kind.is_dir() {
            tar::EntryType::Regular
        } else {
            tar::EntryType::Directory
        };
        rejected(&entries, &config());
        entries.retain(|entry| entry.name != name);
        rejected(&entries, &config());
    }
}

#[test]
fn forbidden_final_layer_members_are_rejected() {
    for name in [
        "homeless-shelter",
        "nix/var/nix/daemon-socket/socket",
        "nix/var/nix/db/reserved",
        "etc/wrix/image-policy.json",
        "lib/libfakeuid.so",
    ] {
        let mut entries = members();
        entries.push(Member::file(name, b""));
        rejected(&entries, &config());
    }
}

#[test]
fn private_marker_bytes_type_and_database_presence_are_checked() {
    let mut entries = members();
    entries
        .iter_mut()
        .find(|entry| entry.name == "etc/wrix/private-nix-version")
        .unwrap()
        .bytes = b"2\n".to_vec();
    rejected(&entries, &config());
    let mut entries = members();
    let marker = entries
        .iter_mut()
        .find(|entry| entry.name == "etc/wrix/private-nix-version")
        .unwrap();
    marker.kind = tar::EntryType::Directory;
    marker.bytes.clear();
    rejected(&entries, &config());
    let mut entries = members();
    entries.retain(|entry| entry.name != "nix/var/nix/db/db.sqlite");
    rejected(&entries, &config());
}

#[test]
fn manifest_requires_exact_direct_root_owned_gc_links() {
    for defect in 0..6 {
        let mut entries = members();
        let link = entries.iter_mut().find(|entry| entry.name == LINK).unwrap();
        match defect {
            0 => link.uid = 1000,
            1 => link.gid = 1000,
            2 => link.link = Some("/nix/store/other".to_owned()),
            3 => {
                link.kind = tar::EntryType::Regular;
                link.link = None;
            }
            4 => entries.retain(|entry| entry.name != LINK),
            _ => entries.push(Member {
                kind: tar::EntryType::Symlink,
                link: Some(ROOT.to_owned()),
                ..Member::file("nix/var/nix/gcroots/wrix-image/unlisted", b"")
            }),
        }
        rejected(&entries, &config());
    }
    let mut entries = members();
    entries
        .iter_mut()
        .find(|entry| entry.name == "etc/wrix/store-roots")
        .unwrap()
        .bytes
        .clear();
    rejected(&entries, &config());
}

#[test]
fn private_labels_environment_and_legacy_policy_are_independent_checks() {
    for change in 0..4 {
        let mut value = config();
        match change {
            0 => value["config"]["Labels"]["wrix.nix.private"] = json!("0"),
            1 => value["config"]["Labels"]["wrix.image.policy"] = json!("anything"),
            2 => value["config"]["Env"] = json!(["IS_SANDBOX=0"]),
            _ => value["config"]["Env"] = json!(["IS_SANDBOX=1", "NIX_REMOTE=daemon"]),
        }
        rejected(&members(), &value);
    }
}

#[test]
fn lower_layer_state_does_not_satisfy_final_customization_contract() {
    let file = fixture(&[], &config(), true);
    assert!(matches!(
        run(file.path(), Format::Archive),
        Err(Error::Contract(_))
    ));
}

#[test]
fn duplicate_final_layer_headers_use_last_metadata() {
    let mut entries = members();
    let mut duplicate = entries
        .iter()
        .find(|entry| entry.name == "nix/store")
        .unwrap()
        .clone();
    duplicate.uid = 0;
    entries.push(duplicate);
    rejected(&entries, &config());
}
