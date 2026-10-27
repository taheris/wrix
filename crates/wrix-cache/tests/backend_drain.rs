#[path = "../../../lib/services/celler-work-drain-state.rs"]
mod native;

#[cfg(test)]
mod test {
    use super::native;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        sync::Arc,
    };
    use wrix_cache::backend::{Client, publication::workflow::Session};

    const ID: &str = "01234567-89ab-4cde-8f01-23456789abcd";

    fn fixture(listener: &TcpListener, tracker: &Arc<native::Tracker>) {
        for operation in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            let mut headers = std::collections::HashMap::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let (name, value) = line.split_once(':').unwrap();
                headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
            }
            let authority = headers.get("authorization").unwrap();
            assert_eq!(authority, "Bearer scoped");
            let id = headers.get("x-wrix-workflow").map(String::as_str);
            let _request = tracker.request(id, authority).unwrap();
            let body = match operation {
                0 => {
                    assert!(first.starts_with("POST /_api/v1/workflow/private "));
                    assert!(id.is_none());
                    assert!(tracker.open_workflow(ID.into(), authority.clone()));
                    format!("{{\"workflow\":\"{ID}\"}}")
                }
                1 => {
                    assert!(first.starts_with("GET /_api/v1/cache-config/private "));
                    assert_eq!(id, Some(ID));
                    assert_eq!(tracker.status().phase, native::Phase::Draining);
                    String::from(
                        "{\"public_key\":\"private:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\"}",
                    )
                }
                2 => {
                    assert!(first.starts_with("DELETE /_api/v1/workflow/private "));
                    assert_eq!(id, Some(ID));
                    assert!(tracker.close_workflow(ID, authority));
                    String::new()
                }
                _ => panic!("unexpected fixture request"),
            };
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    }

    #[tokio::test]
    async fn publication_session_conforms_to_native_workflow_admission_across_http_idle_gap() {
        let tracker = native::Tracker::new();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let serving = tracker.clone();
        let server = std::thread::spawn(move || fixture(&listener, &serving));
        let client = Client::new(
            &endpoint,
            attic::cache::CacheName::new("private".into()).unwrap(),
            "scoped",
        )
        .unwrap();
        let session = Session::open(&client).await.unwrap();
        assert!(tracker.begin());
        assert!(!tracker.release());
        assert!(!tracker.open_workflow("new".into(), "Bearer scoped".into()));
        assert_eq!(
            session.client().public_key().await.unwrap().as_str(),
            "private:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
        );
        session.finish().await.unwrap();
        server.join().unwrap();
        assert!(tracker.release());
        tracker.released();
        assert!(!tracker.abort());
        tracker.fail();
        tracker.background().uncertain();
        assert!(tracker.status().failed);
    }

    #[test]
    fn cancelled_native_request_retains_detached_writes_until_completion() {
        let tracker = native::Tracker::new();
        let request = tracker.request(None, "reader").unwrap();
        let write = tracker.background();
        let maintenance = tracker.maintenance().unwrap();
        tracker.begin();
        drop(request);
        assert!(!tracker.release());
        drop(write);
        drop(maintenance);
        assert!(tracker.quiescent());
        assert!(tracker.release());
    }
}
