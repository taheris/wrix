use super::*;

/// This mocks only the guest stdio peer, not native Apple lifecycle or acceptance.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn paused_guest_backpressure_survives_thirty_seconds_and_observer_loss_without_replay() {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = NonZeroU16::new(listener.local_addr().unwrap().port()).unwrap();
    let mut command = std::process::Command::new("python3");
    command.args([
        "-c",
        r"
import os, struct, time, fcntl
fcntl.fcntl(0, 1031, 4096)
def exact(n):
    data = b''
    while len(data) < n:
        chunk = os.read(0, n-len(data))
        if not chunk: raise RuntimeError('unexpected transport loss')
        data += chunk
    return data
os.write(1, b'WRIXNB01')
assert exact(8) == b'WRIXNB01'
os.write(1, struct.pack('!BII', 1, 1, 0))
time.sleep(31)
kind, stream, length = struct.unpack('!BII', exact(9))
assert (kind, stream, length) == (2, 1, 16384)
assert exact(length) == b'x' * length
os.write(1, struct.pack('!BII', 4, 1, 0))
os.write(1, struct.pack('!BII', 3, 1, 0))
os.write(1, struct.pack('!BII', 1, 2, 0))
time.sleep(120)
",
    ]);
    let (stop, receiver) = oneshot::channel();
    let (ready, startup) = oneshot::channel();
    let task = tokio::spawn(run(command, port, receiver, ready));
    tokio::time::timeout(std::time::Duration::from_secs(5), startup)
        .await
        .unwrap()
        .unwrap();
    let observer = Bridge {
        _epoch: "a".repeat(32).try_into().unwrap(),
    };
    drop(observer);
    let (mut first, _) = listener.accept().await.unwrap();
    first.write_all(&vec![b'x'; protocol::CHUNK]).await.unwrap();
    let second = tokio::time::timeout(std::time::Duration::from_secs(40), listener.accept())
        .await
        .unwrap()
        .unwrap();
    assert!(!task.is_finished());
    stop.send(()).unwrap();
    assert!(task.await.unwrap().is_ok());
    drop(second);
}

#[tokio::test]
async fn native_guest_eof_is_transport_failure_not_clean_lifecycle_completion() {
    let mut command = std::process::Command::new("/bin/sh");
    command.args(["-c", "printf WRIXNB01"]);
    let (_stop, receiver) = oneshot::channel();
    let (ready, _startup) = oneshot::channel();
    assert!(
        run(command, NonZeroU16::new(2222).unwrap(), receiver, ready)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn paused_ssh_receiver_keeps_bounded_flow_control_and_reverse_traffic_without_write_deadline()
{
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    rustix::net::sockopt::set_socket_recv_buffer_size(&listener, 4096).unwrap();
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_send_buffer_size(4096).unwrap();
    let socket = socket
        .connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (mut server, _) = listener.accept().await.unwrap();
    let (tx, rx) = mpsc::channel(2);
    let (outbound, mut frames) = mpsc::channel(2);
    let task = tokio::spawn(async move { forward(socket, 3, rx, &outbound).await });
    let mut sent = 0;
    loop {
        assert!(
            sent < 8 * 1024 * 1024,
            "fixture must actually reach native TCP backpressure"
        );
        tx.send(Frame {
            kind: Kind::Data,
            id: 3,
            data: vec![7; protocol::CHUNK],
        })
        .await
        .unwrap();
        sent += protocol::CHUNK;
        match tokio::time::timeout(std::time::Duration::from_millis(250), frames.recv()).await {
            Ok(Some(frame)) => assert_eq!(frame.kind, Kind::Credit),
            Err(_) => break,
            Ok(None) => panic!("transport exited while acquiring backpressure"),
        }
    }
    server.write_all(b"reverse-traffic").await.unwrap();
    let reverse = tokio::time::timeout(std::time::Duration::from_secs(5), frames.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reverse.kind, Kind::Data);
    assert_eq!(reverse.data, b"reverse-traffic");
    tokio::time::sleep(std::time::Duration::from_secs(31)).await;
    assert!(!task.is_finished());
    rustix::net::sockopt::set_socket_recv_buffer_size(&server, 1024 * 1024).unwrap();
    let mut bytes = vec![0; sent];
    let mut received = 0;
    while received < sent {
        let count = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            server.read(&mut bytes[received..]),
        )
        .await;
        assert!(
            count.is_ok(),
            "stalled at {received} of {sent}; task finished: {}",
            task.is_finished()
        );
        let count = count.unwrap().unwrap();
        assert!(count != 0);
        received += count;
    }
    assert!(bytes.iter().all(|byte| *byte == 7));
    assert_eq!(frames.recv().await.unwrap().kind, Kind::Credit);
    tx.send(Frame::empty(Kind::Close, 3)).await.unwrap();
    assert!(task.await.unwrap().is_ok());
}

#[tokio::test]
async fn fixed_loopback_stream_keeps_ssh_opaque_and_obeys_one_chunk_credit() {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = NonZeroU16::new(listener.local_addr().unwrap().port()).unwrap();
    let (tx, rx) = mpsc::channel(2);
    let (outbound, mut frames) = mpsc::channel(2);
    let task = tokio::spawn(async move { stream(port, 7, rx, &outbound).await });
    let (mut server, peer) = listener.accept().await.unwrap();
    assert!(peer.ip().is_loopback());
    server
        .write_all(b"SSH-2.0-native-server\r\n")
        .await
        .unwrap();
    let first = frames.recv().await.unwrap();
    assert_eq!(first.kind, Kind::Data);
    assert_eq!(first.id, 7);
    assert_eq!(first.data, b"SSH-2.0-native-server\r\n");
    server.write_all(b"second-chunk").await.unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), frames.recv())
            .await
            .is_err()
    );
    tx.send(Frame::empty(Kind::Credit, 7)).await.unwrap();
    let next = frames.recv().await.unwrap();
    assert_eq!(next.data, b"second-chunk");
    tx.send(Frame {
        kind: Kind::Data,
        id: 7,
        data: b"opaque-encrypted-client".to_vec(),
    })
    .await
    .unwrap();
    let mut bytes = [0; 23];
    server.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"opaque-encrypted-client");
    assert_eq!(frames.recv().await.unwrap().kind, Kind::Credit);
    tx.send(Frame::empty(Kind::Close, 7)).await.unwrap();
    assert!(task.await.unwrap().is_ok());
}
