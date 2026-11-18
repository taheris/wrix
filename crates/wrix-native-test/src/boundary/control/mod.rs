use super::Error;
use std::{
    fs,
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpStream},
    path::Path,
    time::Duration,
};

pub(super) fn boot_id(path: &Path, output: &mut impl Write) -> Result<(), Error> {
    output.write_all(fs::read_to_string(path)?.as_bytes())?;
    output.flush()?;
    Ok(())
}

struct Token([u8; 65]);
impl Token {
    fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != 64
            || !bytes
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        {
            return Err(Error::Token);
        }
        let mut token = [0; 65];
        token[..64].copy_from_slice(bytes);
        token[64] = b'\n';
        Ok(Self(token))
    }
}

pub(super) fn stop(root: &Path, port: u16) -> Result<(), Error> {
    let token = Token::parse(&fs::read(root.join("token"))?)?;
    let timeout = Duration::from_secs(3);
    let mut peer =
        TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, port)), timeout)?;
    peer.set_read_timeout(Some(timeout))?;
    peer.set_write_timeout(Some(timeout))?;
    peer.write_all(&token.0)?;
    let mut response = [0; 16];
    let count = peer.read(&mut response)?;
    if &response[..count] != b"ok\n" {
        return Err(Error::Response);
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use std::{io, net::TcpListener, thread};

    #[test]
    fn boot_observation_preserves_exact_text_and_refuses_invalid_encoding() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("boot-id");
        fs::write(&path, "a7c134c8-421d-4985-8a45-ec9104efe9dd\n").unwrap();
        let mut bytes = Vec::new();
        boot_id(&path, &mut bytes).unwrap();
        assert_eq!(bytes, fs::read(&path).unwrap());
        fs::write(&path, [0xff]).unwrap();
        assert!(
            matches!(boot_id(&path, &mut Vec::new()), Err(Error::Io(error)) if error.kind() == io::ErrorKind::InvalidData)
        );
    }

    #[test]
    fn malformed_token_is_refused_without_connecting_or_mutating_control_files() {
        let temporary = tempfile::tempdir().unwrap();
        for token in [b"short".as_slice(), &[b'g'; 64], &[b'A'; 64]] {
            fs::write(temporary.path().join("token"), token).unwrap();
            assert!(matches!(stop(temporary.path(), 0), Err(Error::Token)));
            assert_eq!(fs::read(temporary.path().join("token")).unwrap(), token);
        }
    }

    #[test]
    fn shutdown_requires_exact_acknowledgment_and_sends_only_the_owned_token() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("token"), [b'a'; 64]).unwrap();
        for acknowledgment in [b"no\n".as_slice(), b"ok\nextra", b""] {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            let acknowledgment = acknowledgment.to_vec();
            let server = thread::spawn(move || {
                let (mut peer, _) = listener.accept().unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                let mut token = [0; 65];
                peer.read_exact(&mut token).unwrap();
                assert_eq!(&token[..64], &[b'a'; 64]);
                assert_eq!(token[64], b'\n');
                peer.write_all(&acknowledgment).unwrap();
            });
            assert!(matches!(stop(temporary.path(), port), Err(Error::Response)));
            server.join().unwrap();
            assert!(temporary.path().join("token").exists());
        }
    }
}
