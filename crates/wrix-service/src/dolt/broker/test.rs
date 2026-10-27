use super::*;

#[tokio::test]
async fn closed_gate_rejects_broker_transport_before_request_parsing_or_native_work() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("run")).unwrap();
    let gate = crate::lifecycle::sql::Gate::default();
    assert!(gate.admit_broker().is_err());
    let serving = serve(root.path(), false, gate);
    tokio::pin!(serving);
    let request = async {
        let socket = root.path().join("run/sync.sock");
        while !socket.exists() {
            tokio::task::yield_now().await;
        }
        let mut stream = tokio::net::UnixStream::connect(socket).await.unwrap();
        let mut byte = [0];
        let read = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut byte))
            .await
            .unwrap();
        assert!(matches!(read, Ok(0)));
        assert!(!root.path().join("namespaces").exists());
    };
    tokio::select! {
        result = &mut serving => panic!("broker stopped: {result:?}"),
        () = request => (),
    }
}
