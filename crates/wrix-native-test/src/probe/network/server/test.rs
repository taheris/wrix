use super::*;
#[test]
fn owned_private_service_returns_original_fixture_bytes_and_releases_its_socket() {
    let server = Server::start("127.0.0.1:0".parse().unwrap()).unwrap();
    let address = server.address();
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .write_all(b"GET / HTTP/1.0\r\nHost: fixture\r\n\r\n")
        .unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    assert!(bytes.starts_with(b"HTTP/1.0 200 OK\r\n"));
    assert!(bytes.ends_with(BODY));
    assert!(
        bytes
            .windows(b"Content-Length: 20".len())
            .any(|part| part == b"Content-Length: 20")
    );
    server.finish().unwrap();
    assert!(TcpStream::connect(address).is_err());
}
#[test]
fn invalid_request_is_an_observed_server_error_not_a_success_receipt() {
    let server = Server::start("127.0.0.1:0".parse().unwrap()).unwrap();
    let mut stream = TcpStream::connect(server.address()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(b"POST / HTTP/1.0\r\n\r\n").unwrap();
    let mut bytes = Vec::new();
    match stream.read_to_end(&mut bytes) {
        Ok(_) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionReset | io::ErrorKind::UnexpectedEof
            ) => {}
        Err(error) => panic!("unexpected fixture observation error: {error}"),
    }
    assert_eq!(bytes, b"");
    assert!(server.finish().is_err());
}
