#[test]
fn selected_package_keeps_new_contract_in_verification() {
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_client"))
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(String::from_utf8(result.stdout).unwrap().trim(), "7");
}
