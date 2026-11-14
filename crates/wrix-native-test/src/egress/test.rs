use super::*;
use std::{
    io::Read,
    net::{TcpStream, UdpSocket},
};
fn query(name: &[u8], kind: u16) -> Vec<u8> {
    let mut packet = Vec::new();
    for value in [713_u16, 0x0100, 1, 0, 0, 0] {
        packet.extend_from_slice(&value.to_be_bytes());
    }
    for label in name.split(|byte| *byte == b'.') {
        packet.push(u8::try_from(label.len()).unwrap());
        packet.extend_from_slice(label);
    }
    packet.push(0);
    packet.extend_from_slice(&kind.to_be_bytes());
    packet.extend_from_slice(&1_u16.to_be_bytes());
    packet
}
fn header(reply: &[u8]) -> [u16; 6] {
    std::array::from_fn(|index| word(reply, index * 2).unwrap())
}
#[test]
fn native_a_question_retains_identity_question_and_live_public_control() {
    let question = query(b"pool-public.invalid", 1);
    let reply = dns_reply(&question).unwrap();
    assert_eq!(header(&reply), [713, 0x8180, 1, 1, 0, 0]);
    assert_eq!(&reply[12..question.len()], &question[12..]);
    assert_eq!(&reply[reply.len() - 4..], &[93, 184, 216, 34]);
}
#[test]
fn private_address_is_available_for_native_connect_denial_control() {
    let reply = dns_reply(&query(b"pool-private.invalid", 1)).unwrap();
    assert_eq!(&reply[reply.len() - 4..], &[127, 0, 0, 1]);
}
#[test]
fn ipv6_query_is_nodata_instead_of_a_fabricated_route() {
    assert_eq!(
        header(&dns_reply(&query(b"pool-public.invalid", 28)).unwrap()),
        [713, 0x8180, 1, 0, 0, 0]
    );
}
#[test]
fn unknown_name_is_nxdomain_instead_of_general_dns_forwarding() {
    assert_eq!(
        header(&dns_reply(&query(b"unrelated.invalid", 1)).unwrap()),
        [713, 0x8183, 1, 0, 0, 0]
    );
}
#[test]
fn truncated_response_compression_and_zero_question_packets_are_not_reflected() {
    let question = query(b"pool-public.invalid", 1);
    for packet in [
        vec![],
        question[..question.len() - 1].to_vec(),
        [question[..12].to_vec(), vec![0xc0, 0x0c]].concat(),
        vec![0; 12],
    ] {
        assert!(dns_reply(&packet).is_none());
    }
    let mut response = question;
    response[2] |= 0x80;
    assert!(dns_reply(&response).is_none());
}
#[test]
fn case_insensitive_names_and_question_classes_match_original_dns_semantics() {
    assert_eq!(
        header(&dns_reply(&query(b"POOL-PUBLIC.INVALID", 1)).unwrap()),
        [713, 0x8180, 1, 1, 0, 0]
    );
    let mut question = query(b"pool-public.invalid", 1);
    *question.last_mut().unwrap() = 2;
    assert_eq!(
        header(&dns_reply(&question).unwrap()),
        [713, 0x8183, 1, 0, 0, 0]
    );
}
struct Shutdown<'a>(&'a AtomicBool);
impl Drop for Shutdown<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}
#[test]
fn actual_udp_and_ipv4_ipv6_live_controls_share_bounded_service_lifetime() {
    let bound = Bound::bind(
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0),
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0),
        SocketAddrV6::new(Ipv6Addr::LOCALHOST, 0, 0, 0),
    )
    .unwrap();
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        let server = scope.spawn(|| bound.serve(&stop));
        let _guard = Shutdown(&stop);
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let question = query(b"pool-public.invalid", 1);
        client
            .send_to(&question, bound.dns.local_addr().unwrap())
            .unwrap();
        let mut bytes = [0; 4096];
        let (size, _) = client.recv_from(&mut bytes).unwrap();
        assert_eq!(&bytes[..size], dns_reply(&question).unwrap());
        for address in [
            bound.v4.local_addr().unwrap(),
            bound.v6.local_addr().unwrap(),
        ] {
            let mut peer = TcpStream::connect_timeout(&address, Duration::from_secs(3)).unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut bytes = Vec::new();
            peer.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, b"ok\n");
        }
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap().unwrap();
    });
}
