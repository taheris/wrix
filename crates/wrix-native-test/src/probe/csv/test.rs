use super::*;

#[test]
fn quoted_commas_and_doubled_quotes_match_csv_records() {
    let table = Table::parse(b"name,count\r\n\"a,b\",2\r\n\"q\"\"r\",3\r\n").unwrap();
    assert_eq!(table.column("name").unwrap(), ["a,b", "q\"r"]);
    assert_eq!(table.column("count").unwrap(), ["2", "3"]);
}
#[test]
fn quoted_newlines_remain_in_the_field() {
    let table = Table::parse(b"name,count\n\"a\nb\r\nc\",2").unwrap();
    assert_eq!(table.column("name").unwrap(), ["a\nb\r\nc"]);
}
#[test]
fn final_empty_column_is_not_lost() {
    let table = Table::parse(b"a,b\nx,").unwrap();
    assert_eq!(table.column("b").unwrap(), [""]);
}
#[test]
fn invalid_quote_or_carriage_return_fails_closed() {
    for bytes in [
        b"a\n\"unfinished".as_slice(),
        b"a\nx\"",
        b"a\n\"x\"y",
        b"a\rx",
        b"a\n\"x\"\r",
    ] {
        assert!(Table::parse(bytes).is_err(), "{bytes:?}");
    }
}
#[test]
fn duplicate_or_empty_headers_are_refused() {
    for bytes in [b"a,a\n1,2".as_slice(), b"a,\n1,2", b"", b"\n"] {
        assert!(matches!(Table::parse(bytes), Err(Error::Header)));
    }
}
#[test]
fn ambiguous_row_width_is_refused() {
    for bytes in [b"a,b\n1".as_slice(), b"a\n1,2"] {
        assert!(matches!(Table::parse(bytes), Err(Error::Syntax)));
    }
}
#[test]
fn field_and_document_bounds_are_enforced() {
    assert!(matches!(
        Table::parse(&vec![b'x'; 1024 * 1024 + 1]),
        Err(Error::Oversized)
    ));
    let mut bytes = b"a\n".to_vec();
    bytes.extend(std::iter::repeat_n(b'x', 65_537));
    assert!(matches!(Table::parse(&bytes), Err(Error::Oversized)));
}
#[test]
fn non_utf8_input_is_not_lossily_accepted() {
    assert!(matches!(Table::parse(b"a\n\xff"), Err(Error::Encoding)));
}
