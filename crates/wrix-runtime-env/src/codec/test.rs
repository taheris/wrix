use super::*;

#[test]
fn literal_wire_conformance_preserves_empty_multiline_unicode_and_equals() {
    let payload = Payload::from_pairs([
        ("EMPTY".into(), String::new()),
        ("MULTI".into(), "first\nsecond=tail\n".into()),
        ("UNICODE".into(), "café".into()),
    ])
    .unwrap();
    let mut bytes = Vec::new();
    payload.write(&mut bytes).unwrap();
    let expected =
        "wrix-runtime-env-v1\0EMPTY\0\0MULTI\0first\nsecond=tail\n\0UNICODE\0café\0".as_bytes();
    assert_eq!(bytes, expected);
    let values: Vec<_> = Payload::decode(expected)
        .unwrap()
        .environment()
        .map(|(name, value)| (name.to_owned(), value.into_vec()))
        .collect();
    assert_eq!(
        values,
        vec![
            ("EMPTY".to_owned(), vec![]),
            ("MULTI".to_owned(), b"first\nsecond=tail\n".to_vec()),
            ("UNICODE".to_owned(), "café".as_bytes().to_vec())
        ]
    );
}

#[test]
fn duplicate_names_keep_their_order_for_last_wins_execution() {
    let payload = Payload::decode(b"wrix-runtime-env-v1\0KEY\0first\0KEY\0last\0").unwrap();
    let values: Vec<_> = payload.environment().collect();
    assert_eq!(
        values,
        [
            ("KEY", OsString::from("first")),
            ("KEY", OsString::from("last"))
        ]
    );
}

#[test]
fn binary_values_are_opaque_but_names_are_ascii_identifiers() {
    let payload = Payload::decode(b"wrix-runtime-env-v1\0TOKEN\0\xff\xfe\0").unwrap();
    assert_eq!(
        payload.environment().next().unwrap().1.into_vec(),
        b"\xff\xfe"
    );
    for name in ["", "1KEY", "BAD=KEY", "BAD\nKEY", "BAD\0KEY", "kéy"] {
        assert!(Payload::from_pairs([(name.into(), "synthetic-private-value".into())]).is_err());
    }
    assert!(Payload::from_pairs([("KEY".into(), "bad\0value".into())]).is_err());
}

#[test]
fn truncated_unknown_and_odd_records_are_rejected_without_echoing_contents() {
    for bytes in [
        b"".as_slice(),
        b"other\0TOKEN\0synthetic-private-value\0",
        b"wrix-runtime-env-v1\0TOKEN",
        b"wrix-runtime-env-v1\0TOKEN\0",
        b"wrix-runtime-env-v1\0TOKEN\0value",
        b"wrix-runtime-env-v1\0TOKEN\0value\0ODD\0",
        b"wrix-runtime-env-v1\0\0value\0",
    ] {
        let error = Payload::decode(bytes).unwrap_err();
        assert!(!error.to_string().contains("synthetic-private-value"));
        assert!(matches!(error, Error::Invalid));
    }
}

#[test]
fn zero_variables_is_valid_and_debug_is_redacted() {
    assert_eq!(
        Payload::decode(b"wrix-runtime-env-v1\0")
            .unwrap()
            .environment()
            .count(),
        0
    );
    let payload =
        Payload::from_pairs([("TOKEN".into(), "synthetic-private-value".into())]).unwrap();
    let debug = format!("{payload:?}");
    assert!(!debug.contains("TOKEN"));
    assert!(!debug.contains("synthetic-private-value"));
    assert!(debug.contains("[REDACTED]"));
}

#[test]
fn byte_and_entry_limits_fail_closed() {
    assert!(matches!(
        Payload::from_pairs([("KEY".into(), "x".repeat(MAX_BYTES))]),
        Err(Error::TooLarge)
    ));
    assert!(matches!(
        Payload::decode(&vec![0; MAX_BYTES + 1]),
        Err(Error::TooLarge)
    ));
    assert!(matches!(
        Payload::from_pairs((0..=MAX_ENTRIES).map(|_| ("A".into(), String::new()))),
        Err(Error::TooLarge)
    ));
    let mut wire = MAGIC.to_vec();
    for _ in 0..=MAX_ENTRIES {
        wire.extend_from_slice(b"A\0\0");
    }
    assert!(matches!(Payload::decode(&wire), Err(Error::TooLarge)));
}

#[test]
fn read_and_write_failures_are_not_accepted_as_empty_payloads() {
    struct Failure;
    impl Read for Failure {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("injected read failure"))
        }
    }
    impl Write for Failure {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("injected write failure"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("injected flush failure"))
        }
    }
    assert!(
        Payload::read(Failure)
            .unwrap_err()
            .to_string()
            .contains("injected read failure")
    );
    let payload = Payload::decode(MAGIC).unwrap();
    assert!(
        payload
            .write(&mut Failure)
            .unwrap_err()
            .to_string()
            .contains("injected write failure")
    );
}
