pub fn source() -> [String; 4] {
    [r#"
    fn load_project_cache() {
        let url = format!("http://{sandbox_host}:{}", endpoint.port);
        let nix_config = format!(
            "extra-substituters = {url}\nextra-trusted-public-keys = {}\nbuilders-use-substitutes = true",
            public_key.as_str()
        );
    }
    fn load_dolt() {}
"#.into(), r#"
pub(in crate::lifecycle) fn ensure() {
        let layout = Layout::current()?;
        format!(
            "127.0.0.1:{}:8080",
            shared.cache_port(selected.port)
        );
        for mount in layout.mounts() {}
        .arg("wrix-shared-service")
}
pub(in crate::lifecycle) fn sql_ready() {}
"#.into(), r#"
    let mut celler = tokio::process::Command::new("sh")
        .args(["-c", wrix_cache::backend::layout::runtime::COMMAND]);
    let mut terminate = signal();
"#.into(), r#"
  stableContents = [
    celler
  ];
  contents = stableContents ++ pkgs.lib.optional (pool == null) supervisor;
  baseImage = {
    contents = stableContents;
  };
  rawImage = {
    contents = if pool == null then [ supervisor ] else contents;
  };
  sourceKind = "docker-archive";
"#.into()]
}
