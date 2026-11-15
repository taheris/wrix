use super::*;
fn observed(packets: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"nftables":[{"rule":{"family":"inet","table":"wrix_npfo4_egress","comment":"core-probe","expr":[{"counter":{"packets":packets,"bytes":64}}]}}]})).unwrap()
}
#[test]
fn owned_kernel_observation_requires_positive_exact_integer_packets() {
    let count = Count::parse(&observed(&serde_json::json!(7))).unwrap();
    assert_eq!(count.packets(), 7);
    for data in [
        serde_json::json!(-1),
        serde_json::json!(1.5),
        serde_json::json!("7"),
        serde_json::Value::Null,
    ] {
        assert!(matches!(
            Count::parse(&observed(&data)),
            Err(Error::Structure)
        ));
    }
    assert!(matches!(
        Count::parse(&observed(&serde_json::json!(0))),
        Err(Error::Zero)
    ));
}
#[test]
fn foreign_table_or_comment_cannot_supply_private_service_drop_evidence() {
    let mut value: serde_json::Value =
        serde_json::from_slice(&observed(&serde_json::json!(8))).unwrap();
    value["nftables"][0]["rule"]["table"] = serde_json::json!("neighbor");
    assert!(matches!(
        Count::parse(&serde_json::to_vec(&value).unwrap()),
        Err(Error::Missing)
    ));
    value["nftables"][0]["rule"]["table"] = serde_json::json!("wrix_npfo4_egress");
    value["nftables"][0]["rule"]["comment"] = serde_json::json!("unrelated");
    assert!(matches!(
        Count::parse(&serde_json::to_vec(&value).unwrap()),
        Err(Error::Missing)
    ));
}
#[test]
fn malformed_target_counters_are_not_silently_ignored_beside_valid_ones() {
    let mut value: serde_json::Value =
        serde_json::from_slice(&observed(&serde_json::json!(1))).unwrap();
    value["nftables"][0]["rule"]["expr"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"counter":{"packets":"bad"}}));
    assert!(matches!(
        Count::parse(&serde_json::to_vec(&value).unwrap()),
        Err(Error::Structure)
    ));
    value["nftables"][0]["rule"]["expr"] =
        serde_json::json!([{"counter":{"packets":u64::MAX}},{"counter":{"packets":1}}]);
    assert!(matches!(
        Count::parse(&serde_json::to_vec(&value).unwrap()),
        Err(Error::Overflow)
    ));
}
#[test]
fn subsequent_nix_observation_must_strictly_increase_the_same_owned_counter() {
    let before = Count::parse(&observed(&serde_json::json!(2))).unwrap();
    let after = Count::parse(&observed(&serde_json::json!(3))).unwrap();
    after.increased_from(before).unwrap();
    assert!(matches!(
        before.increased_from(before),
        Err(Error::Unchanged)
    ));
    assert!(matches!(
        before.increased_from(after),
        Err(Error::Unchanged)
    ));
}
