use std::{
    io::Write,
    net::{TcpListener, TcpStream},
    time::{Duration, Instant},
};

#[test]
fn stalled_body_writes_obey_the_response_deadline() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    socket2::SockRef::from(&client)
        .set_recv_buffer_size(4096)
        .unwrap();
    let (mut stream, _) = listener.accept().unwrap();
    socket2::SockRef::from(&stream)
        .set_send_buffer_size(4096)
        .unwrap();
    let start = Instant::now();
    let mut output = super::Output {
        stream: &mut stream,
        deadline: start + Duration::from_millis(100),
    };
    let error = output.write_all(&vec![0; 1024 * 1024]).unwrap_err();
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ));
    assert!(start.elapsed() < Duration::from_secs(2));
}
