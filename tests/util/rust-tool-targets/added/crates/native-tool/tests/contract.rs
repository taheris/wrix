#[test]
fn runs_the_selected_tool() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_native-tool"))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"7\n");
}
