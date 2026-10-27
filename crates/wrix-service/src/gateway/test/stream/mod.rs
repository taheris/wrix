use super::*;

#[tokio::test]
async fn large_http_response_streams_with_fixed_client_and_origin_buffers() {
    let fixture = Fixture::start().await;
    let secret = Secret::generate().unwrap();
    let _grant = fixture.grant(&secret).await;
    let mut socket = fixture.connect().await;
    assert!(
        request(&mut socket, &secret, "allowed.example", "GET", "/large")
            .await
            .starts_with("HTTP/1.1 200 ")
    );
    let mut chunk = [0; 8192];
    for _ in 0..128 {
        socket.read_exact(&mut chunk).await.unwrap();
        assert!(chunk.iter().all(|b| *b == b'x'));
    }
    drop(socket);
    fixture.finish().await;
}

#[tokio::test]
async fn connect_preserves_prefetched_binary_bytes_without_tls_interception() {
    let fixture = Fixture::start().await;
    let secret = Secret::generate().unwrap();
    let _grant = fixture.grant(&secret).await;
    let mut socket = fixture.connect().await;
    let mut bytes = format!("CONNECT allowed.example:80 HTTP/1.1\r\nHost: allowed.example:80\r\nProxy-Authorization: Bearer {}\r\n\r\n",secret.expose()).into_bytes();
    let opaque = b"\x16\x03\x03\x00\xff\x80\r\n\r\n";
    bytes.extend_from_slice(opaque);
    socket.write_all(&bytes).await.unwrap();
    assert!(header(&mut socket).await.starts_with("HTTP/1.1 200 "));
    let mut echoed = [0; 10];
    socket.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, opaque);
    fixture.finish().await;
}

#[tokio::test]
async fn revocation_cancels_pending_numeric_connect() {
    let fixture = Fixture::start().await;
    fixture.network.dial_pending.store(true, Ordering::SeqCst);
    let secret = Secret::generate().unwrap();
    let grant = fixture.grant(&secret).await;
    let mut socket = fixture.connect().await;
    socket.write_all(format!("GET http://allowed.example/ HTTP/1.1\r\nHost: allowed.example\r\nProxy-Authorization: Bearer {}\r\n\r\n",secret.expose()).as_bytes()).await.unwrap();
    fixture.network.dialing.notified().await;
    drop(grant);
    revoked(&mut socket).await;
    assert_eq!(fixture.network.connections.load(Ordering::SeqCst), 0);
    fixture.finish().await;
}

#[tokio::test]
async fn revocation_closes_upload_while_origin_waits_for_remaining_body() {
    let fixture = Fixture::start().await;
    let secret = Secret::generate().unwrap();
    let grant = fixture.grant(&secret).await;
    let mut socket = fixture.connect().await;
    socket.write_all(format!("POST http://allowed.example/upload HTTP/1.1\r\nHost: allowed.example\r\nProxy-Authorization: Bearer {}\r\nContent-Length: 10000000\r\n\r\npartial",secret.expose()).as_bytes()).await.unwrap();
    for _ in 0..100 {
        if fixture.network.connections.load(Ordering::SeqCst) != 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(fixture.network.connections.load(Ordering::SeqCst), 1);
    drop(grant);
    revoked(&mut socket).await;
    fixture.finish().await;
}

#[tokio::test]
async fn connection_capacity_rejects_excess_without_interrupting_admitted_peer() {
    let fixture = Fixture::capacity(1).await;
    let secret = Secret::generate().unwrap();
    let _grant = fixture.grant(&secret).await;
    let mut first = fixture.connect().await;
    get(&mut first, &secret).await;
    let mut excess = fixture.connect().await;
    eof(&mut excess).await;
    get(&mut first, &secret).await;
    assert_eq!(fixture.management.status().await.unwrap().connections, 1);
    fixture.finish().await;
}

#[tokio::test]
async fn basic_proxy_url_authentication_matches_supported_tool_contract() {
    use base64::Engine;
    let fixture = Fixture::start().await;
    let secret = Secret::generate().unwrap();
    let _grant = fixture.grant(&secret).await;
    let mut socket = fixture.connect().await;
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(format!("wrix:{}", secret.expose()));
    socket.write_all(format!("GET http://allowed.example/ HTTP/1.1\r\nHost: allowed.example\r\nProxy-Authorization: Basic {encoded}\r\n\r\n").as_bytes()).await.unwrap();
    assert!(header(&mut socket).await.starts_with("HTTP/1.1 200 "));
    fixture.finish().await;
}

#[tokio::test]
async fn canonicalized_chunked_framing_cannot_smuggle_unadmitted_next_request() {
    let fixture = Fixture::start().await;
    let secret = Secret::generate().unwrap();
    let _grant = fixture.grant(&secret).await;
    let mut socket = fixture.connect().await;
    socket.write_all(format!("GET http://allowed.example/ HTTP/1.1\r\nHost: allowed.example\r\nProxy-Authorization: Bearer {}\r\nTransfer-Encoding: chunked\r\nContent-Length: 1000\r\n\r\n0\r\n\r\n",secret.expose()).as_bytes()).await.unwrap();
    assert!(header(&mut socket).await.starts_with("HTTP/1.1 200 "));
    let mut body = [0; 5];
    socket.read_exact(&mut body).await.unwrap();
    assert_eq!(&body, b"hello");
    let denied = request(&mut socket, &secret, "denied.example", "GET", "/").await;
    assert!(
        denied.is_empty() || denied.starts_with("HTTP/1.1 403 "),
        "{denied}"
    );
    assert_eq!(fixture.network.connections.load(Ordering::SeqCst), 1);
    fixture.finish().await;
}

#[tokio::test]
async fn keepalive_cannot_switch_grant_or_couple_independent_revocation() {
    let fixture = Fixture::start().await;
    let first = Secret::generate().unwrap();
    let _first_grant = fixture.grant(&first).await;
    let second = Secret::generate().unwrap();
    let _second_grant = fixture.grant(&second).await;
    let mut socket = fixture.connect().await;
    get(&mut socket, &first).await;
    assert!(
        request(&mut socket, &second, "allowed.example", "GET", "/")
            .await
            .starts_with("HTTP/1.1 407 ")
    );
    get(&mut fixture.connect().await, &second).await;
    fixture.finish().await;
}

#[tokio::test]
async fn explicitly_listed_private_management_destination_is_still_protected() {
    let fixture = Fixture::start().await;
    let secret = Secret::generate().unwrap();
    let private = fixture.management_address;
    let policy = policy::Policy::new(vec![
        policy::Rule::new(
            policy::Host::parse("127.0.0.1").unwrap(),
            vec![NonZeroU16::new(private.port()).unwrap()],
        )
        .unwrap(),
    ])
    .unwrap();
    let _grant = fixture
        .management
        .attach(secret.clone(), policy)
        .await
        .unwrap();
    *fixture.network.answers.lock().unwrap() = vec![private];
    let mut socket = fixture.connect().await;
    socket.write_all(format!("CONNECT {private} HTTP/1.1\r\nHost: {private}\r\nProxy-Authorization: Bearer {}\r\n\r\n",secret.expose()).as_bytes()).await.unwrap();
    assert!(header(&mut socket).await.starts_with("HTTP/1.1 403 "));
    assert_eq!(fixture.network.connections.load(Ordering::SeqCst), 0);
    assert!(fixture.management.status().await.is_ok());
    fixture.finish().await;
}

#[tokio::test]
async fn allowed_hostname_does_not_imply_unlisted_port() {
    let fixture = Fixture::start().await;
    let secret = Secret::generate().unwrap();
    let _grant = fixture.grant(&secret).await;
    let mut socket = fixture.connect().await;
    assert!(
        request(&mut socket, &secret, "allowed.example:81", "GET", "/")
            .await
            .starts_with("HTTP/1.1 403 ")
    );
    assert_eq!(fixture.network.connections.load(Ordering::SeqCst), 0);
    fixture.finish().await;
}

#[tokio::test]
async fn prepared_native_grant_has_no_authority_until_private_confirmation() {
    let fixture = Fixture::start().await;
    let secret = Secret::generate().unwrap();
    let mut candidate = fixture
        .management
        .prepare(secret.clone(), policy())
        .await
        .unwrap();
    assert!(
        request(
            &mut fixture.connect().await,
            &secret,
            "allowed.example",
            "GET",
            "/"
        )
        .await
        .starts_with("HTTP/1.1 407 ")
    );
    candidate.activate().await.unwrap();
    get(&mut fixture.connect().await, &secret).await;
    candidate.release().await.unwrap();
    assert!(
        request(
            &mut fixture.connect().await,
            &secret,
            "allowed.example",
            "GET",
            "/"
        )
        .await
        .starts_with("HTTP/1.1 407 ")
    );
    assert!(fixture.management.prepare(secret, policy()).await.is_err());
    fixture.finish().await;
}

#[test]
fn parsed_policy_rejects_unknown_fields_empty_ports_and_ambiguous_hosts() {
    for raw in [
        r#"[{"host":"allowed.example","ports":[]}]"#,
        r#"[{"host":"allowed.example","ports":[80],"all":true}]"#,
        r#"[{"host":"allowed.example","ports":[0]}]"#,
        r#"[{"host":"allowed.example.","ports":[80]}]"#,
        r#"[{"host":"2130706433","ports":[80]}]"#,
        r#"[{"host":"127.1","ports":[80]}]"#,
    ] {
        assert!(
            serde_json::from_str::<policy::Policy>(raw).is_err(),
            "{raw}"
        );
    }
}
