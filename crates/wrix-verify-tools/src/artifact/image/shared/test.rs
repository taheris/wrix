use super::*;
use serde_json::json;
use std::io::{Seek, SeekFrom, Write};

const FIRST: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-first";
const SECOND: &str = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-second";

fn append(builder: &mut tar::Builder<Vec<u8>>, name: &str, bytes: &[u8], directory: bool) {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o755);
    header.set_entry_type(if directory {
        tar::EntryType::Directory
    } else {
        tar::EntryType::Regular
    });
    header.set_cksum();
    builder.append_data(&mut header, name, bytes).unwrap();
}

struct Fixture {
    root: tempfile::TempDir,
    case: Case,
    config: Value,
}

impl Fixture {
    fn new() -> Self {
        Self::with_extra(&[])
    }

    fn with_extra(entries: &[(&str, &[u8], bool)]) -> Self {
        let root = tempfile::tempdir().unwrap();
        let seed_closure = root.path().join("seed");
        fs::write(&seed_closure, format!("{FIRST}\n{SECOND}\n")).unwrap();
        let shared = Shared {
            version: Capability::One,
            seed_closure,
        };
        let config = json!({"config": {"Labels": {
            "wrix.nix.private":"1", "wrix.nix.shared":"1",
            "wrix.nix.seed":format!("{:x}", Sha256::digest(shared.seed_closure.to_str().unwrap().as_bytes()))
        }, "Env":["IS_SANDBOX=1", "VALUE=with=equals"]}});
        let database = root.path().join("database");
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch("CREATE TABLE ValidPaths(path TEXT NOT NULL)")
            .unwrap();
        for path in [FIRST, SECOND] {
            connection
                .execute("INSERT INTO ValidPaths VALUES (?)", [path])
                .unwrap();
        }
        drop(connection);
        let mut layer = tar::Builder::new(Vec::new());
        for path in [FIRST, SECOND] {
            append(&mut layer, &path[1..], b"", true);
        }
        for marker in [
            "etc/wrix/private-nix-version",
            "etc/wrix/shared-nix-version",
        ] {
            append(&mut layer, marker, b"1\n", false);
        }
        append(
            &mut layer,
            "nix/var/nix/db/db.sqlite",
            &fs::read(database).unwrap(),
            false,
        );
        let layer = layer.into_inner().unwrap();
        let source = root.path().join("image.tar");
        let mut archive = tar::Builder::new(Vec::new());
        append(
            &mut archive,
            "config.json",
            &serde_json::to_vec(&config).unwrap(),
            false,
        );
        append(&mut archive, "layer.tar", &layer, false);
        let mut layers = vec!["layer.tar"];
        if !entries.is_empty() {
            let mut upper = tar::Builder::new(Vec::new());
            for &(name, bytes, directory) in entries {
                append(&mut upper, name, bytes, directory);
            }
            append(
                &mut archive,
                "upper.tar",
                &upper.into_inner().unwrap(),
                false,
            );
            layers.push("upper.tar");
        }
        append(
            &mut archive,
            "manifest.json",
            &serde_json::to_vec(&json!([{"Config":"config.json","Layers":layers}])).unwrap(),
            false,
        );
        fs::write(&source, archive.into_inner().unwrap()).unwrap();
        let artifact = root.path().join("inspection");
        Image::prepare(&source, &artifact).unwrap();
        Self {
            root,
            config,
            case: Case {
                artifact,
                source,
                source_kind: SourceKind::DockerArchive,
                shared,
                required_paths: vec![FIRST.to_owned()],
                profile_config: None,
                digest_file: None,
            },
        }
    }

    fn config(&self, value: &Value) {
        fs::write(
            self.case.artifact.join("config.json"),
            serde_json::to_vec(value).unwrap(),
        )
        .unwrap();
    }

    fn replace(&self, name: &str, bytes: &[u8]) {
        let image = Image::prepared(&self.case.artifact).unwrap();
        let (_, header) = image.lookup(&Text::from_utf8(name)).unwrap().unwrap();
        assert_eq!(bytes.len() as u64, header.size);
        let image_source = image.source.as_ref().unwrap();
        let base = image_source.1[&image.layers[0]].0;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(&self.case.source)
            .unwrap();
        file.seek(SeekFrom::Start(base + header.offset_data))
            .unwrap();
        file.write_all(bytes).unwrap();
    }
}

#[test]
fn prepared_manifest_rejects_multiple_images_and_config_substitution() {
    let fixture = Fixture::new();
    for manifest in [
        json!([]),
        json!([{"Config":"config.json","Layers":["layer.tar"]},{"Config":"config.json","Layers":["layer.tar"]}]),
        json!([{"Config":"../other-config.json","Layers":["layer.tar"]}]),
    ] {
        fs::write(
            fixture.case.artifact.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert!(Image::prepared(&fixture.case.artifact).is_err());
    }
}

#[test]
fn real_archive_markers_store_seed_and_readonly_database_agree() {
    let fixture = Fixture::new();
    verify(&fixture.case).unwrap();
    assert!(fixture.case.artifact.join("archive.json").is_file());
    assert!(!fixture.case.artifact.join("layer.tar").exists());
}

#[test]
fn labels_and_environment_fail_independently_when_corrupted() {
    let fixture = Fixture::new();
    for key in ["wrix.nix.private", "wrix.nix.shared", "wrix.nix.seed"] {
        let mut config = fixture.config.clone();
        config["config"]["Labels"][key] = json!("wrong");
        fixture.config(&config);
        assert!(
            matches!(verify(&fixture.case), Err(Error::Contract(_))),
            "{key}"
        );
    }
    for env in [
        json!([]),
        json!(["IS_SANDBOX=0"]),
        json!(["IS_SANDBOX=1", "NIX_REMOTE=daemon"]),
    ] {
        let mut config = fixture.config.clone();
        config["config"]["Env"] = env;
        fixture.config(&config);
        assert!(matches!(verify(&fixture.case), Err(Error::Contract(_))));
    }
}

#[test]
fn seed_uniqueness_materialization_and_required_paths_are_checked() {
    let fixture = Fixture::new();
    for contents in [
        String::new(),
        format!("{FIRST}\n{FIRST}\n"),
        format!("{FIRST}\n"),
    ] {
        fs::write(&fixture.case.shared.seed_closure, contents).unwrap();
        assert!(matches!(verify(&fixture.case), Err(Error::Contract(_))));
    }
    fs::write(
        &fixture.case.shared.seed_closure,
        format!("{FIRST}\n{SECOND}\n"),
    )
    .unwrap();
    let mut missing = fixture.case;
    missing
        .required_paths
        .push("/nix/store/cccccccccccccccccccccccccccccccc-missing".to_owned());
    assert!(matches!(verify(&missing), Err(Error::Contract(_))));
}

#[test]
fn both_contract_marker_payloads_are_verified() {
    for name in [
        "etc/wrix/private-nix-version",
        "etc/wrix/shared-nix-version",
    ] {
        let fixture = Fixture::new();
        fixture.replace(name, b"2\n");
        assert!(matches!(verify(&fixture.case), Err(Error::Contract(_))));
    }
}

#[test]
fn registration_is_not_inferred_from_seed_or_headers() {
    let fixture = Fixture::new();
    let database = fixture.root.path().join("wrong-db");
    let image = Image::prepared(&fixture.case.artifact).unwrap();
    extract(&image, "nix/var/nix/db/db.sqlite", &database).unwrap();
    let connection = Connection::open(&database).unwrap();
    connection
        .execute("DELETE FROM ValidPaths WHERE path = ?", [SECOND])
        .unwrap();
    drop(connection);
    fixture.replace("nix/var/nix/db/db.sqlite", &fs::read(database).unwrap());
    assert!(matches!(
        verify(&fixture.case),
        Err(Error::Contract(
            "seed and private image registration disagree"
        ))
    ));
}

#[test]
fn forbidden_effective_members_are_not_accepted() {
    for member in ["homeless-shelter", "nix/var/nix/daemon-socket/socket"] {
        let fixture = Fixture::with_extra(&[(member, b"", false)]);
        assert!(matches!(
            verify(&fixture.case),
            Err(Error::Contract("forbidden effective image member present"))
        ));
    }
}

#[test]
fn whiteouted_store_roots_are_not_counted_as_materialized() {
    let fixture = Fixture::with_extra(&[(
        "nix/store/.wh.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-first",
        b"",
        false,
    )]);
    assert!(matches!(
        verify(&fixture.case),
        Err(Error::Contract("seed differs from materialized store"))
    ));
}

#[test]
fn shared_schema_rejects_missing_extra_boolean_and_incompatible_capabilities() {
    for shared in [
        json!({"version":true,"seed_closure":"seed"}),
        json!({"version":2,"seed_closure":"seed"}),
        json!({"version":1}),
        json!({"version":1,"seed_closure":"seed","extra":1}),
    ] {
        assert!(serde_json::from_value::<Shared>(shared).is_err());
    }
}

#[test]
fn descriptor_and_profile_identity_fields_are_all_checked() {
    let fixture = Fixture::new();
    let mut case = fixture.case.clone();
    case.source_kind = SourceKind::NixDescriptor;
    case.source = fixture.root.path().join("descriptor.json");
    let config_bytes = fs::read(case.artifact.join("config.json")).unwrap();
    let descriptor = json!({"oci_layout":"/real/layout", "shared_nix":case.shared,
        "config":{"labels":fixture.config["config"]["Labels"]},
        "digest":format!("sha256:{:x}", Sha256::digest(config_bytes))});
    fs::write(&case.source, serde_json::to_vec(&descriptor).unwrap()).unwrap();
    verify(&case).unwrap();
    for pointer in [
        "/shared_nix/version",
        "/config/labels/wrix.nix.private",
        "/digest",
    ] {
        let mut invalid = descriptor.clone();
        *invalid.pointer_mut(pointer).unwrap() = json!("wrong");
        fs::write(&case.source, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(matches!(verify(&case), Err(Error::Contract(_))));
    }
    fs::write(&case.source, serde_json::to_vec(&descriptor).unwrap()).unwrap();
    let profile = fixture.root.path().join("profile.json");
    let digest = fixture.root.path().join("digest");
    fs::write(&digest, "sha256:profile\n").unwrap();
    case.profile_config = Some(profile.clone());
    case.digest_file = Some(digest);
    let valid = json!({"image":{"shared_nix":case.shared,"source":case.source,
        "source_kind":"nix-descriptor","digest":"sha256:profile"}});
    fs::write(&profile, serde_json::to_vec(&valid).unwrap()).unwrap();
    verify(&case).unwrap();
    for pointer in [
        "/image/shared_nix/version",
        "/image/source",
        "/image/source_kind",
        "/image/digest",
    ] {
        let mut invalid = valid.clone();
        *invalid.pointer_mut(pointer).unwrap() = json!("wrong");
        fs::write(&profile, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(matches!(verify(&case), Err(Error::Contract(_))));
    }
}

#[test]
fn complete_driver_runs_negative_controls_after_valid_contracts() {
    let fixture = Fixture::new();
    let cases = fixture.root.path().join("cases.json");
    fs::write(&cases, serde_json::to_vec(&vec![&fixture.case]).unwrap()).unwrap();
    assert_eq!(run(&cases, Path::new("true")).unwrap(), 1);
}

#[test]
fn permission_verifier_failure_cannot_be_reported_as_contract_success() {
    let fixture = Fixture::new();
    let cases = fixture.root.path().join("cases.json");
    fs::write(&cases, serde_json::to_vec(&vec![&fixture.case]).unwrap()).unwrap();
    assert!(matches!(
        run(&cases, Path::new("false")),
        Err(Error::Permission(_))
    ));
}

#[test]
fn permission_boundary_uses_archive_flag_or_actual_descriptor_layout() {
    let fixture = Fixture::new();
    let archive = permission_command(&fixture.case, Path::new("permissions")).unwrap();
    assert_eq!(
        archive.get_args().collect::<Vec<_>>(),
        [
            std::ffi::OsStr::new("--archive"),
            fixture.case.source.as_os_str()
        ]
    );
    let mut case = fixture.case.clone();
    case.source_kind = SourceKind::NixDescriptor;
    case.source = fixture.root.path().join("descriptor");
    fs::write(&case.source, br#"{"oci_layout":"/declared/layout"}"#).unwrap();
    let descriptor = permission_command(&case, Path::new("permissions")).unwrap();
    assert_eq!(
        descriptor.get_args().collect::<Vec<_>>(),
        [std::ffi::OsStr::new("/declared/layout")]
    );
}
