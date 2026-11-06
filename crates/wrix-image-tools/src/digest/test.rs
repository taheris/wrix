mod property;

use super::*;

fn raw_hash(bytes: &[u8]) -> ConfigDigest {
    ConfigDigest(Sha256::digest(bytes))
}

// Ported from test_config_digest.py: all three original scenarios remain.
#[test]
fn indentation_is_ignored() {
    let compact = br#"{"rootfs":{"type":"layers","diff_ids":[]}}"#;
    let pretty =
        "{\n    \"rootfs\": {\n        \"type\": \"layers\",\n        \"diff_ids\": []\n    }\n}";
    assert_eq!(config_digest(pretty.as_bytes()).unwrap(), raw_hash(compact));
}

#[test]
fn string_whitespace_and_escapes_are_retained() {
    let compact = r#"{"Env":["SPACES=two  words","QUOTE=\" ","PATH=a\\b","ESCAPED=\u003c\u0026\u003e","UNICODE=λ"]}"#;
    let pretty = compact.replace("{\"Env\":", "{\n    \"Env\": ");
    assert_eq!(
        config_digest(pretty.as_bytes()).unwrap(),
        raw_hash(compact.as_bytes())
    );
}

#[test]
fn invalid_json_is_not_hashed() {
    for invalid in [
        b"{\"broken\":".as_slice(),
        b"NaN",
        b"Infinity",
        b"null trailing",
        b"\"unterminated",
    ] {
        assert!(config_digest(invalid).is_err());
    }
}

#[test]
fn numbers_escapes_and_key_order_are_not_normalized() {
    for input in [
        br#"{"z":-0,"a":1e+00}"#.as_slice(),
        br#"{"s":"\u003c","slash":"\/"}"#,
    ] {
        assert_eq!(config_digest(input).unwrap(), raw_hash(input));
    }
    assert_ne!(
        config_digest(br#"{"n":1.0}"#).unwrap(),
        config_digest(br#"{"n":1}"#).unwrap()
    );
    assert_ne!(
        config_digest(br#"{"z":0,"a":1}"#).unwrap(),
        config_digest(br#"{"a":1,"z":0}"#).unwrap()
    );
}

#[test]
fn escaped_backslashes_do_not_end_strings_early() {
    for compact in [
        br#"{"s":"\\", "t":" x "}"#.as_slice(),
        br#"{"s":"\\\"  x", "t":"\\\\"}"#,
    ] {
        let expected = compact
            .iter()
            .enumerate()
            .filter_map(|(index, byte)| {
                // These two fixtures have exactly one insignificant space after ','.
                if *byte == b' ' && index > 0 && compact[index - 1] == b',' {
                    None
                } else {
                    Some(*byte)
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(config_digest(compact).unwrap(), raw_hash(&expected));
    }
}
