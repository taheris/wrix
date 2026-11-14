use rustix::net::{AddressFamily, SocketFlags, SocketType, ipproto};
use std::{
    io::{self, Write},
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6, TcpListener, UdpSocket},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

#[derive(Debug, thiserror::Error, displaydoc::Display)]
pub enum Error {
    /// egress fixture socket I/O failed: {0}
    Io(#[from] io::Error),
    /// egress fixture socket setup failed: {0}
    Socket(#[from] rustix::io::Errno),
    /// egress fixture worker panicked
    Worker,
}
fn word(bytes: &[u8], index: usize) -> Option<u16> {
    Some(u16::from_be_bytes([
        *bytes.get(index)?,
        *bytes.get(index + 1)?,
    ]))
}
pub fn dns_reply(packet: &[u8]) -> Option<Vec<u8>> {
    if packet.len() < 12 {
        return None;
    }
    let identity = word(packet, 0)?;
    let flags = word(packet, 2)?;
    if flags & 0xf800 != 0 || word(packet, 4)? != 1 {
        return None;
    }
    let mut cursor = 12;
    let mut name = Vec::new();
    loop {
        let length = usize::from(*packet.get(cursor)?);
        cursor += 1;
        if length == 0 {
            break;
        }
        if length > 63 {
            return None;
        }
        let label = packet.get(cursor..cursor + length)?;
        if !name.is_empty() {
            name.push(b'.');
        }
        name.extend(label.iter().map(u8::to_ascii_lowercase));
        cursor += length;
    }
    let kind = word(packet, cursor)?;
    let family = word(packet, cursor + 2)?;
    cursor += 4;
    let address = if family == 1 {
        match name.as_slice() {
            b"pool-public.invalid" => Some([93, 184, 216, 34]),
            b"pool-private.invalid" => Some([127, 0, 0, 1]),
            _ => None,
        }
    } else {
        None
    };
    let answer = if kind == 1 { address } else { None };
    let response_flags = 0x8080 | (flags & 0x0100) | if address.is_some() { 0 } else { 3 };
    let mut response = Vec::new();
    for value in [
        identity,
        response_flags,
        1,
        u16::from(answer.is_some()),
        0,
        0,
    ] {
        response.extend_from_slice(&value.to_be_bytes());
    }
    response.extend_from_slice(&packet[12..cursor]);
    if let Some(address) = answer {
        response.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 1, 0, 4]);
        response.extend_from_slice(&address);
    }
    Some(response)
}
fn listener(address: SocketAddr) -> Result<TcpListener, Error> {
    let family = if address.is_ipv4() {
        AddressFamily::INET
    } else {
        AddressFamily::INET6
    };
    let flags = SocketFlags::empty();
    #[cfg(not(target_vendor = "apple"))]
    let flags = flags | SocketFlags::CLOEXEC;
    let socket = rustix::net::socket_with(family, SocketType::STREAM, flags, Some(ipproto::TCP))?;
    rustix::io::fcntl_setfd(&socket, rustix::io::FdFlags::CLOEXEC)?;
    rustix::net::sockopt::set_socket_reuseaddr(&socket, true)?;
    if address.is_ipv6() {
        rustix::net::sockopt::set_ipv6_v6only(&socket, true)?;
    }
    rustix::net::bind(&socket, &address)?;
    rustix::net::listen(&socket, 16)?;
    let listener = TcpListener::from(socket);
    listener.set_nonblocking(true)?;
    Ok(listener)
}
pub struct Bound {
    dns: UdpSocket,
    v4: TcpListener,
    v6: TcpListener,
}
impl Bound {
    pub fn bind(dns: SocketAddrV4, v4: SocketAddrV4, v6: SocketAddrV6) -> Result<Self, Error> {
        let dns = UdpSocket::bind(dns)?;
        dns.set_read_timeout(Some(Duration::from_millis(100)))?;
        let v4 = listener(v4.into())?;
        let v6 = listener(v6.into())?;
        Ok(Self { dns, v4, v6 })
    }
    pub fn serve(&self, stop: &AtomicBool) -> Result<(), Error> {
        let errors = thread::scope(|scope| {
            let dns = scope.spawn(|| {
                let result = self.serve_dns(stop);
                stop.store(true, Ordering::Relaxed);
                result
            });
            let v6 = scope.spawn(|| {
                let result = Self::serve_tcp(&self.v6, stop);
                stop.store(true, Ordering::Relaxed);
                result
            });
            let v4 = Self::serve_tcp(&self.v4, stop);
            stop.store(true, Ordering::Relaxed);
            (v4, v6.join(), dns.join())
        });
        errors.0?;
        errors.1.map_err(|_| Error::Worker)??;
        errors.2.map_err(|_| Error::Worker)??;
        Ok(())
    }
    fn serve_dns(&self, stop: &AtomicBool) -> Result<(), Error> {
        let mut bytes = [0; 4096];
        while !stop.load(Ordering::Relaxed) {
            match self.dns.recv_from(&mut bytes) {
                Ok((count, peer)) => {
                    if let Some(reply) = dns_reply(&bytes[..count]) {
                        self.dns.send_to(&reply, peer)?;
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
    fn serve_tcp(listener: &TcpListener, stop: &AtomicBool) -> Result<(), Error> {
        while !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    socket.set_write_timeout(Some(Duration::from_secs(3)))?;
                    socket.write_all(b"ok\n")?;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}
pub fn run() -> Result<(), Error> {
    Bound::bind(
        SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 53),
        SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 18_443),
        SocketAddrV6::new(Ipv6Addr::LOCALHOST, 18_443, 0, 0),
    )?
    .serve(&AtomicBool::new(false))
}
#[cfg(test)]
mod test;
