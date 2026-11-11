use super::*;
use tempfile::TempDir;

const STORE: &str = "nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-fixture";

enum Payload<'a> {
    File(&'a [u8]),
    Link(u8, &'a str),
}

fn tar(entries: &[(&str, Payload<'_>, u32)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, payload, mode) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_mode(*mode);
        header.set_path(name.trim_start_matches('/')).unwrap();
        match payload {
            Payload::File(bytes) => {
                header.set_size(bytes.len() as u64);
                header.set_cksum();
                builder.append(&header, *bytes).unwrap();
            }
            Payload::Link(kind, target) => {
                header.set_entry_type(tar::EntryType::new(*kind));
                header.set_link_name(target).unwrap();
                header.set_size(0);
                header.set_cksum();
                builder.append(&header, io::empty()).unwrap();
            }
        }
    }
    builder.into_inner().unwrap()
}

struct Fixture {
    temp: TempDir,
    layout: PathBuf,
    layers: Vec<Vec<u8>>,
    descriptors: Vec<Json>,
    image: Image,
    selected: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let layout = temp.path().join("layout");
        fs::create_dir_all(layout.join("blobs/sha256")).unwrap();
        let layers = vec![
            tar(&[
                (
                    &format!("{STORE}/bin/runner"),
                    Payload::File(b"#!/bin/sh\necho original\n"),
                    0o755,
                ),
                (
                    "./bin/runner",
                    Payload::Link(b'2', &format!("/{STORE}/bin/runner")),
                    0o777,
                ),
                ("etc/marker", Payload::File(b"old\n"), 0o444),
                ("etc/removed", Payload::File(b"removed\n"), 0o444),
                ("opt/hidden/file", Payload::File(b"hidden\n"), 0o444),
            ]),
            tar(&[
                ("etc/marker", Payload::File(b"new\n"), 0o444),
                ("etc/.wh.removed", Payload::File(b""), 0o444),
                ("opt/hidden/.wh..wh..opq", Payload::File(b""), 0o444),
                ("etc/marker-link", Payload::Link(b'1', "etc/marker"), 0o444),
            ]),
        ];
        let descriptors = layers
            .iter()
            .map(|data| Self::blob(&layout, data))
            .collect();
        let placeholder = Image {
            root: temp.path().join("unused"),
            layers: Vec::new(),
            members: BTreeMap::new(),
            digests: BTreeMap::new(),
            source: None,
        };
        let selected = temp.path().join("selected");
        let mut fixture = Self {
            temp,
            layout,
            layers,
            descriptors,
            image: placeholder,
            selected,
        };
        fixture.rebuild("inspection");
        fixture
    }

    fn blob(layout: &Path, data: &[u8]) -> Json {
        let hex = format!("{:x}", Sha256::digest(data));
        fs::write(layout.join("blobs/sha256").join(&hex), data).unwrap();
        object([
            ("digest", text(&format!("sha256:{hex}"))),
            ("size", number(data.len() as u64)),
            ("mediaType", text("application/vnd.oci.image.layer.v1.tar")),
        ])
    }

    fn index(&self) {
        let config = Self::blob(
            &self.layout,
            br#"{"config":{"Entrypoint":["/entrypoint.sh"]}}"#,
        );
        let manifest = object([
            ("config", config),
            ("layers", Json::Array(self.descriptors.clone())),
        ]);
        let descriptor = Self::blob(&self.layout, manifest.encode().as_bytes());
        write_json(
            &self.layout.join("index.json"),
            &object([("manifests", Json::Array(vec![descriptor]))]),
        )
        .unwrap();
    }

    fn rebuild(&mut self, name: &str) {
        self.index();
        self.image = Image::prepare(&self.layout, &self.temp.path().join(name)).unwrap();
        let mut lines = Vec::new();
        for layer in &self.image.layers {
            lines.extend(layer.bytes().unwrap());
            lines.push(b'\n');
        }
        fs::write(&self.selected, lines).unwrap();
    }

    fn append(&mut self, entries: &[(&str, Payload<'_>, u32)]) {
        let data = tar(entries);
        self.descriptors.push(Self::blob(&self.layout, &data));
        self.layers.push(data);
        self.rebuild("changed");
    }

    fn query(&self, operation: &str, member: Option<&str>) -> (bool, Vec<u8>) {
        let mut args = vec![
            operation.into(),
            self.image.root.as_os_str().to_owned(),
            self.selected.as_os_str().to_owned(),
        ];
        if let Some(member) = member {
            args.push(member.into());
        }
        let mut output = Vec::new();
        let found = Image::run(&args, &mut output).unwrap();
        (found, output)
    }

    fn extract(&self, member: &str) -> Vec<u8> {
        let output = self.temp.path().join("extracted");
        let image = Image::load(&self.image.root, &self.selected).unwrap();
        assert!(image.extract(&Text::from_utf8(member), &output).unwrap());
        fs::read(output).unwrap()
    }

    fn docker(&self, extra: &[(&str, Payload<'_>, u32)]) -> Vec<u8> {
        let mut entries: Vec<_> = self
            .layers
            .iter()
            .enumerate()
            .map(|(index, data)| (format!("layer-{index}"), data.as_slice()))
            .collect();
        entries.push(("original-config.json".into(), br#"{"config":{}}"#));
        let manifest = Json::Array(vec![object([
            ("Config", text("original-config.json")),
            (
                "Layers",
                Json::Array(
                    (0..self.layers.len())
                        .map(|index| text(&format!("layer-{index}")))
                        .collect(),
                ),
            ),
        ])])
        .encode();
        entries.push(("manifest.json".into(), manifest.as_bytes()));
        let mut all: Vec<_> = entries
            .iter()
            .filter(|(name, _)| !extra.iter().any(|(key, _, _)| *key == name))
            .map(|(name, data)| (name.as_str(), Payload::File(data), 0o644))
            .collect();
        for (name, data, mode) in extra {
            let data = match data {
                Payload::File(data) => Payload::File(data),
                Payload::Link(kind, name) => Payload::Link(*kind, name),
            };
            all.push((name, data, *mode));
        }
        tar(&all)
    }

    fn docker_image(&self, bytes: &[u8], name: &str) -> io::Result<Image> {
        let source = self.temp.path().join(format!("{name}.tar"));
        fs::write(&source, bytes)?;
        Image::prepare(&source, &self.temp.path().join(name))
    }
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

fn sparse_layer(version: &str) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_path("sparse").unwrap();
    header.set_mode(0o640);
    if version == "gnu" {
        header.set_entry_type(tar::EntryType::new(b'S'));
        header.set_size(3);
        let bytes = header.as_mut_bytes();
        bytes[386..398].copy_from_slice(b"00000000004\0");
        bytes[398..410].copy_from_slice(b"00000000003\0");
        bytes[483..495].copy_from_slice(b"00000000024\0");
        header.set_cksum();
        builder.append(&header, b"abc".as_slice()).unwrap();
    } else {
        let size_key = if version == "0.0" {
            "GNU.sparse.size"
        } else {
            "GNU.sparse.realsize"
        };
        let mut attributes = vec![(size_key, b"20".as_slice())];
        match version {
            "0.0" => attributes.extend([
                ("GNU.sparse.offset", b"4".as_slice()),
                ("GNU.sparse.numbytes", b"3".as_slice()),
            ]),
            "0.1" => attributes.push(("GNU.sparse.map", b"4,3")),
            _ => attributes.extend([
                ("GNU.sparse.major", b"1".as_slice()),
                ("GNU.sparse.minor", b"0".as_slice()),
            ]),
        }
        builder.append_pax_extensions(attributes).unwrap();
        let mut data = Vec::new();
        if version == "1.0" {
            data.extend_from_slice(b"1\n4\n3\n");
            data.resize(512, 0);
        }
        data.extend_from_slice(b"abc");
        header.set_size(data.len() as u64);
        header.set_cksum();
        builder.append(&header, data.as_slice()).unwrap();
    }
    builder.into_inner().unwrap()
}

#[test]
fn sparse_gnu_and_all_pax_versions_retain_offsets_maps_and_logical_bytes() {
    for version in ["gnu", "0.0", "0.1", "1.0"] {
        let mut fixture = Fixture::new();
        let data = sparse_layer(version);
        fixture.layers = vec![data.clone()];
        fixture.descriptors = vec![Fixture::blob(&fixture.layout, &data)];
        fixture.rebuild("sparse");
        let header = &fixture.image.members[&fixture.image.layers[0]][&Text::from_utf8("sparse")];
        assert_eq!(header.size, 20);
        assert_eq!(header.offset, 0);
        let expected = match version {
            "gnu" => 512,
            "1.0" => 2048,
            _ => 1536,
        };
        assert_eq!(header.offset_data, expected);
        let offset = usize::try_from(expected).unwrap();
        assert_eq!(&data[offset..offset + 3], b"abc");
        let mut expected = vec![0; 20];
        expected[4..7].copy_from_slice(b"abc");
        assert_eq!(fixture.extract("sparse"), expected);
        let image = fixture
            .docker_image(&gzip(&fixture.docker(&[])), "sparse-docker")
            .unwrap();
        let output = fixture.temp.path().join("sparse-docker-output");
        assert!(image.extract(&Text::from_utf8("sparse"), &output).unwrap());
        assert_eq!(fs::read(output).unwrap(), expected);
    }
}

#[test]
fn byte_names_roundtrip_json_queries_and_extracted_payloads_without_replacement() {
    let mut fixture = Fixture::new();
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.as_mut_bytes()[..5].copy_from_slice(b"etc/\xff");
    header.set_size(3);
    header.set_mode(0o755);
    header.set_cksum();
    builder.append(&header, b"raw".as_slice()).unwrap();
    let mut store = STORE.as_bytes().to_vec();
    store.push(255);
    let mut name = store.clone();
    name.extend_from_slice(b"/bin/payload");
    header.as_mut_bytes()[..100].fill(0);
    header.as_mut_bytes()[..name.len()].copy_from_slice(&name);
    header.set_cksum();
    builder.append(&header, b"raw".as_slice()).unwrap();
    let data = builder.into_inner().unwrap();
    fixture.descriptors = vec![Fixture::blob(&fixture.layout, &data)];
    fixture.rebuild("byte-names");
    let loaded = Image::load(&fixture.image.root, &fixture.selected).unwrap();
    let name = Text::from_bytes(b"etc/\xff");
    assert!(loaded.lookup(&name).unwrap().is_some());
    let output = fixture.temp.path().join("raw-output");
    assert!(loaded.extract(&name, &output).unwrap());
    assert_eq!(fs::read(output).unwrap(), b"raw");
    assert!(
        fs::read_to_string(fixture.image.root.join("members.json"))
            .unwrap()
            .contains("\\udcff")
    );
    assert_eq!(
        fixture.query("find", Some("etc/removed")),
        (false, b"\n".to_vec())
    );
    let mut expected = vec![b'/'];
    expected.extend(store);
    expected.push(b'\n');
    assert_eq!(fixture.query("paths", None), (true, expected));
}

#[test]
fn cyclic_missing_and_symlink_hardlink_targets_fail_before_creating_output() {
    let mut fixture = Fixture::new();
    fixture.append(&[
        ("cycle-a", Payload::Link(b'1', "cycle-b"), 0o444),
        ("cycle-b", Payload::Link(b'1', "cycle-a"), 0o444),
        ("missing-target", Payload::Link(b'1', "absent"), 0o444),
        ("symlink", Payload::Link(b'2', "/etc/passwd"), 0o777),
        ("nonregular", Payload::Link(b'1', "symlink"), 0o444),
    ]);
    for member in ["cycle-a", "missing-target", "nonregular"] {
        let output = fixture.temp.path().join(member);
        assert!(
            fixture
                .image
                .extract(&Text::from_utf8(member), &output)
                .is_err()
        );
        assert!(!output.exists());
    }
}

#[test]
fn duplicate_normalized_outer_members_and_duplicate_layer_references_are_rejected() {
    let fixture = Fixture::new();
    let mut data = fixture.docker(&[]);
    data.truncate(data.len() - 1024);
    data.extend(tar(&[(
        "./layer-0",
        Payload::File(&fixture.layers[0]),
        0o644,
    )]));
    assert!(
        fixture
            .docker_image(&data, "duplicate-member")
            .unwrap_err()
            .to_string()
            .contains("duplicate Docker archive member")
    );
    let data = fixture.docker(&[(
        "manifest.json",
        Payload::File(br#"[{"Config":"original-config.json","Layers":["layer-0","layer-0"]}]"#),
        0o644,
    )]);
    assert!(
        fixture
            .docker_image(&data, "duplicate-layer")
            .unwrap_err()
            .to_string()
            .contains("duplicate Docker image layer")
    );
}

#[test]
fn tampered_index_size_cannot_extract_beyond_docker_layer_bounds() {
    let fixture = Fixture::new();
    let mut image = fixture
        .docker_image(&fixture.docker(&[]), "bounds")
        .unwrap();
    let first = image.layers[0].clone();
    let entry = image
        .members
        .get_mut(&first)
        .unwrap()
        .get_mut(&Text::from_utf8(&format!("{STORE}/bin/runner")))
        .unwrap();
    entry.size = u64::MAX;
    let output = fixture.temp.path().join("out-of-bounds");
    assert!(
        image
            .extract(&Text::from_utf8(&format!("{STORE}/bin/runner")), &output)
            .is_err()
    );
    assert!(!output.exists());
}

#[test]
fn descriptor_preparation_links_emitted_blobs_without_copying_them() {
    let fixture = Fixture::new();
    assert!(fixture.image.root.join("blobs").is_symlink());
    assert!(fixture.image.root.join("config.json").is_symlink());
    assert_eq!(fixture.image.members.len(), 2);
}

#[test]
fn paths_are_discovered_from_headers_without_reopening_blobs() {
    let fixture = Fixture::new();
    fs::rename(&fixture.layout, fixture.temp.path().join("offline")).unwrap();
    assert_eq!(
        fixture.query("paths", None),
        (true, format!("/{STORE}\n").into_bytes())
    );
}

#[test]
fn extraction_preserves_emitted_bytes_and_executable_mode() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.extract(&format!("{STORE}/bin/runner")),
        b"#!/bin/sh\necho original\n"
    );
    assert_eq!(
        fixture
            .temp
            .path()
            .join("extracted")
            .metadata()
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}

#[test]
fn empty_header_queries_retain_python_print_newlines_and_failure_status() {
    let fixture = Fixture::new();
    fs::write(&fixture.selected, b"").unwrap();
    assert_eq!(fixture.query("paths", None), (true, b"\n".to_vec()));
    assert_eq!(
        fixture.query("find", Some("absent")),
        (false, b"\n".to_vec())
    );
    assert_eq!(fixture.query("exists", Some("absent")), (false, Vec::new()));
}

#[test]
fn last_layer_overrides_earlier_file_contents() {
    assert_eq!(Fixture::new().extract("etc/marker"), b"new\n");
}

#[test]
fn whiteout_removes_lower_layer_member() {
    assert!(!Fixture::new().query("exists", Some("etc/removed")).0);
}

#[test]
fn find_reports_emitted_headers_even_when_whiteouts_hide_files() {
    assert_eq!(
        Fixture::new().query("find", Some("etc/removed")),
        (true, b"etc/removed\n".to_vec())
    );
}

#[test]
fn directory_whiteout_hides_lower_layer_descendants() {
    let mut fixture = Fixture::new();
    fixture.append(&[("nix/.wh.store", Payload::File(b""), 0o444)]);
    assert!(
        !fixture
            .query("exists", Some(&format!("{STORE}/bin/runner")))
            .0
    );
}

#[test]
fn whiteout_does_not_hide_recreated_file_in_same_layer() {
    let mut fixture = Fixture::new();
    fixture.append(&[
        ("etc/.wh.removed", Payload::File(b""), 0o444),
        ("etc/removed", Payload::File(b"recreated\n"), 0o444),
    ]);
    assert_eq!(fixture.extract("etc/removed"), b"recreated\n");
}

#[test]
fn opaque_directory_hides_lower_layer_children() {
    assert!(!Fixture::new().query("exists", Some("opt/hidden/file")).0);
}

#[test]
fn selected_tier_queries_exclude_unselected_layers() {
    let fixture = Fixture::new();
    fs::write(&fixture.selected, fixture.image.layers[0].bytes().unwrap()).unwrap();
    assert!(fixture.query("exists", Some("etc/removed")).0);
    assert_eq!(fixture.extract("etc/marker"), b"old\n");
}

#[test]
fn symlink_target_is_read_from_emitted_header() {
    assert_eq!(
        Fixture::new().query("link", Some("bin/runner")),
        (true, format!("/{STORE}/bin/runner\n").into_bytes())
    );
}

#[test]
fn hardlink_extraction_resolves_indexed_target() {
    assert_eq!(Fixture::new().extract("etc/marker-link"), b"new\n");
}

#[test]
fn missing_member_fails_without_creating_output() {
    let fixture = Fixture::new();
    let output = fixture.temp.path().join("missing");
    assert!(
        !fixture
            .image
            .extract(&Text::from_utf8("absent"), &output)
            .unwrap()
    );
    assert!(!output.exists());
}

#[test]
fn same_size_layer_corruption_is_rejected_before_indexing() {
    let fixture = Fixture::new();
    let filename = blob(&fixture.layout, &fixture.descriptors[0]).unwrap();
    let mut bytes = fs::read(&filename).unwrap();
    let end = bytes.len();
    bytes[end - 1] = b'x';
    fs::write(filename, bytes).unwrap();
    assert!(
        Image::prepare(&fixture.layout, &fixture.temp.path().join("bad"))
            .unwrap_err()
            .to_string()
            .contains("digest mismatch")
    );
}

#[test]
fn layer_size_mismatch_is_rejected() {
    let mut fixture = Fixture::new();
    let Json::Object(fields) = &mut fixture.descriptors[0] else {
        panic!("descriptor");
    };
    fields.insert(Text::from_utf8("size"), number(1));
    fixture.index();
    assert!(
        Image::prepare(&fixture.layout, &fixture.temp.path().join("bad"))
            .unwrap_err()
            .to_string()
            .contains("size mismatch")
    );
}

fn unsafe_tar() -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.as_mut_bytes()[..10].copy_from_slice(b"../outside");
    header.set_size(3);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append(&header, b"bad".as_slice()).unwrap();
    builder.into_inner().unwrap()
}

#[test]
fn unsafe_archive_member_is_rejected() {
    let mut fixture = Fixture::new();
    fixture.descriptors = vec![Fixture::blob(&fixture.layout, &unsafe_tar())];
    fixture.index();
    assert!(Image::prepare(&fixture.layout, &fixture.temp.path().join("bad")).is_err());
    assert!(!fixture.temp.path().join("outside").exists());
}

#[test]
fn docker_archives_keep_payloads_in_original_source() {
    let fixture = Fixture::new();
    let raw = fixture.docker(&[]);
    let split = [&gzip(&raw[..529])[..], &gzip(&raw[529..])[..]].concat();
    for (index, bytes) in [raw.clone(), gzip(&raw), split].iter().enumerate() {
        let image = fixture
            .docker_image(bytes, &format!("docker-{index}"))
            .unwrap();
        assert!(!image.root.join("layer-0").exists());
        assert!(
            image
                .root
                .read_dir()
                .unwrap()
                .map(|entry| entry.unwrap().metadata().unwrap().len())
                .sum::<u64>()
                < 8192
        );
        let output = fixture.temp.path().join(format!("docker-output-{index}"));
        assert!(
            image
                .extract(&Text::from_utf8("etc/marker-link"), &output)
                .unwrap()
        );
        assert_eq!(fs::read(output).unwrap(), b"new\n");
        assert_eq!(
            image.digests[&Text::from_utf8("layer-0")],
            text(&format!("sha256:{:x}", Sha256::digest(&fixture.layers[0])))
        );
        assert!(
            image
                .lookup(&Text::from_utf8("etc/removed"))
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn gzip_member_boundaries_can_split_tar_headers_and_payloads() {
    let fixture = Fixture::new();
    let raw = fixture.docker(&[]);
    let baseline = fixture.docker_image(&raw, "raw").unwrap();
    let cuts = [0, 1, 511, 529, 512 + fixture.layers[0].len() - 7, raw.len()];
    let compressed: Vec<_> = cuts
        .windows(2)
        .flat_map(|pair| gzip(&raw[pair[0]..pair[1]]))
        .collect();
    let image = fixture.docker_image(&compressed, "split").unwrap();
    for filename in [
        "config.json",
        "manifest.json",
        "members.json",
        "layer-digests.json",
    ] {
        assert_eq!(
            fs::read(image.root.join(filename)).unwrap(),
            fs::read(baseline.root.join(filename)).unwrap()
        );
    }
}

#[test]
fn later_gzip_members_do_not_hide_unsafe_outer_headers() {
    let fixture = Fixture::new();
    let mut raw = fixture.docker(&[]);
    raw.truncate(raw.len() - 1024);
    raw.extend(unsafe_tar());
    let split = [&gzip(&raw[..529])[..], &gzip(&raw[529..])[..]].concat();
    assert!(fixture.docker_image(&split, "bad").is_err());
}

#[test]
fn later_gzip_member_checksum_corruption_is_rejected() {
    let fixture = Fixture::new();
    let raw = fixture.docker(&[]);
    let mut split = [&gzip(&raw[..529])[..], &gzip(&raw[529..])[..]].concat();
    let end = split.len();
    split[end - 8] ^= 1;
    assert!(fixture.docker_image(&split, "bad").is_err());
}

#[test]
fn inspection_disk_does_not_grow_with_layer_payload() {
    let mut fixture = Fixture::new();
    let bytes = vec![b'x'; 4 * 1024 * 1024];
    fixture.layers = vec![tar(&[("large", Payload::File(&bytes), 0o444)])];
    let image = fixture
        .docker_image(&gzip(&fixture.docker(&[])), "bounded")
        .unwrap();
    assert!(
        image
            .root
            .read_dir()
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum::<u64>()
            < 8192
    );
}

#[test]
fn docker_outer_path_traversal_is_rejected_without_writing_payloads() {
    let fixture = Fixture::new();
    let mut raw = unsafe_tar();
    raw.truncate(raw.len() - 1024);
    raw.extend(fixture.docker(&[]));
    assert!(fixture.docker_image(&raw, "bad").is_err());
    assert!(!fixture.temp.path().join("outside").exists());
}

#[test]
fn docker_inner_path_traversal_is_rejected() {
    let mut fixture = Fixture::new();
    fixture.layers = vec![unsafe_tar()];
    assert!(fixture.docker_image(&fixture.docker(&[]), "bad").is_err());
}

#[test]
fn nonregular_docker_metadata_and_layers_are_rejected() {
    let fixture = Fixture::new();
    for (index, name) in ["original-config.json", "layer-0"].iter().enumerate() {
        let bytes = fixture.docker(&[(name, Payload::Link(b'2', "/etc/passwd"), 0o777)]);
        assert!(
            fixture
                .docker_image(&bytes, &format!("bad-{index}"))
                .is_err()
        );
    }
}

#[test]
fn missing_docker_layer_is_rejected() {
    let fixture = Fixture::new();
    let bytes = fixture.docker(&[(
        "manifest.json",
        Payload::File(br#"[{"Config":"original-config.json","Layers":["missing-layer"]}]"#),
        0o644,
    )]);
    assert!(
        fixture
            .docker_image(&bytes, "bad")
            .unwrap_err()
            .to_string()
            .contains("missing Docker image layer")
    );
}

#[test]
fn multiple_docker_manifests_are_rejected() {
    let fixture = Fixture::new();
    let bytes = fixture.docker(&[("manifest.json", Payload::File(b"[{},{}]"), 0o644)]);
    assert!(
        fixture
            .docker_image(&bytes, "bad")
            .unwrap_err()
            .to_string()
            .contains("exactly one Docker manifest")
    );
}

#[test]
fn oversized_docker_metadata_is_rejected() {
    let fixture = Fixture::new();
    let data = vec![b' '; 16 * 1024 * 1024 + 1];
    let bytes = fixture.docker(&[("original-config.json", Payload::File(&data), 0o644)]);
    assert!(
        fixture
            .docker_image(&bytes, "bad")
            .unwrap_err()
            .to_string()
            .contains("oversized image metadata")
    );
}

#[test]
fn stream_producer_is_snapshotted_once_without_unpacking_layers() {
    let fixture = Fixture::new();
    let source = fixture.temp.path().join("source.tar");
    fs::write(&source, fixture.docker(&[])).unwrap();
    let counter = fixture.temp.path().join("count");
    let producer = fixture.temp.path().join("producer");
    let shell = Command::new("sh")
        .arg("-c")
        .arg("command -v sh")
        .output()
        .unwrap();
    assert!(shell.status.success());
    let shell = String::from_utf8(shell.stdout).unwrap();
    fs::write(
        &producer,
        format!(
            "#!{}\nset -eu\nprintf 'run\\n' >> '{}'\nexec cat '{}'\n",
            shell.trim(),
            counter.display(),
            source.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&producer, fs::Permissions::from_mode(0o755)).unwrap();
    let image = Image::prepare(&producer, &fixture.temp.path().join("stream")).unwrap();
    let output = fixture.temp.path().join("stream-output");
    assert!(
        image
            .extract(&Text::from_utf8("etc/marker"), &output)
            .unwrap()
    );
    assert_eq!(fs::read(output).unwrap(), b"new\n");
    assert_eq!(fs::read(counter).unwrap(), b"run\n");
    assert!(image.root.join("archive.tar").is_file());
    assert!(!image.root.join("layer-0").exists());
}
