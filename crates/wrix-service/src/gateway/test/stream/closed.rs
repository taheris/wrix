//! Only callers whose assertions already permit closure may use this reader.
use super::*;
use std::io::ErrorKind;
use tokio::io::AsyncRead;

pub(super) async fn request_or_closed(socket: &mut TcpStream, secret: &Secret) -> String {
    let request = format!(
        "GET http://denied.example/ HTTP/1.1\r\nHost: denied.example\r\nProxy-Authorization: Bearer {}\r\n\r\n",
        secret.expose()
    );
    match socket.write_all(request.as_bytes()).await {
        Ok(()) => (),
        Err(error) if closed(error.kind()) => (),
        Err(error) => panic!("unexpected denied request write: {error}"),
    }
    String::from_utf8(optional_header(socket).await).unwrap()
}

fn closed(kind: ErrorKind) -> bool {
    matches!(kind, ErrorKind::ConnectionReset | ErrorKind::BrokenPipe)
}

async fn optional_header(reader: &mut (impl AsyncRead + Unpin)) -> Vec<u8> {
    // Preserve the ordinary header reader's original deadline and size bound.
    timeout(Duration::from_secs(3), async {
        let mut result = Vec::new();
        loop {
            let mut byte = [0];
            match reader.read(&mut byte).await {
                Ok(0) => break,
                Ok(_) => result.push(byte[0]),
                Err(error) if closed(error.kind()) => break,
                Err(error) => panic!("unexpected optional header read: {error}"),
            }
            assert!(result.len() < 32768);
            if result.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        result
    })
    .await
    .unwrap()
}

struct EndsWithError {
    bytes: &'static [u8],
    error: ErrorKind,
}
impl AsyncRead for EndsWithError {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if let Some((byte, rest)) = this.bytes.split_first() {
            buffer.put_slice(&[*byte]);
            this.bytes = rest;
            std::task::Poll::Ready(Ok(()))
        } else {
            std::task::Poll::Ready(Err(std::io::Error::from(this.error)))
        }
    }
}

#[tokio::test]
async fn reset_before_response_is_equivalent_to_the_already_permitted_eof() {
    let mut reset = EndsWithError {
        bytes: b"",
        error: ErrorKind::ConnectionReset,
    };
    assert_eq!(optional_header(&mut reset).await, b"");
    assert_eq!(optional_header(&mut &b""[..]).await, b"");
}

#[tokio::test]
async fn reset_never_discards_partial_success_bytes_to_fabricate_an_empty_denial() {
    let mut reset = EndsWithError {
        bytes: b"HTTP/1.1 200 ",
        error: ErrorKind::ConnectionReset,
    };
    let bytes = optional_header(&mut reset).await;
    assert_eq!(bytes, b"HTTP/1.1 200 ");
    assert!(!bytes.is_empty() && !bytes.starts_with(b"HTTP/1.1 403 "));
}

#[tokio::test]
async fn complete_success_response_is_not_hidden_by_a_later_reset() {
    let mut reset = EndsWithError {
        bytes: b"HTTP/1.1 200 OK\r\n\r\n",
        error: ErrorKind::ConnectionReset,
    };
    assert_eq!(
        optional_header(&mut reset).await,
        b"HTTP/1.1 200 OK\r\n\r\n"
    );
}

#[tokio::test]
#[should_panic(expected = "unexpected optional header read")]
async fn nonclosure_errors_are_still_failures() {
    let mut denied = EndsWithError {
        bytes: b"",
        error: ErrorKind::PermissionDenied,
    };
    optional_header(&mut denied).await;
}
