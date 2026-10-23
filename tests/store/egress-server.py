import socket
import struct
import sys
import threading
import unittest


def dns_reply(packet):
    if len(packet) < 12:
        return None
    identity, flags, questions, _, _, _ = struct.unpack("!6H", packet[:12])
    if flags & 0xF800 or questions != 1:
        return None
    cursor = 12
    labels = []
    while cursor < len(packet):
        length = packet[cursor]
        cursor += 1
        if length == 0:
            break
        if length > 63 or cursor + length > len(packet):
            return None
        labels.append(packet[cursor : cursor + length].lower())
        cursor += length
    else:
        return None
    if cursor + 4 > len(packet):
        return None
    kind, family = struct.unpack("!2H", packet[cursor : cursor + 4])
    question = packet[12 : cursor + 4]
    addresses = {
        b"pool-public.invalid": "93.184.216.34",
        b"pool-private.invalid": "127.0.0.1",
    }
    address = addresses.get(b".".join(labels)) if family == 1 else None
    known = address is not None
    answer = b""
    if known and kind == 1:
        answer = struct.pack("!HHHIH", 0xC00C, 1, 1, 1, 4) + socket.inet_aton(address)
    response_flags = 0x8080 | (flags & 0x0100) | (0 if known else 3)
    header = struct.pack("!6H", identity, response_flags, 1, int(bool(answer)), 0, 0)
    return header + question + answer


def serve_dns():
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as listener:
        listener.bind(("0.0.0.0", 53))
        while True:
            packet, peer = listener.recvfrom(4096)
            reply = dns_reply(packet)
            if reply is not None:
                listener.sendto(reply, peer)


def serve(address, family):
    listener = socket.socket(family, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    if family == socket.AF_INET6:
        listener.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
    listener.bind((address, 18443))
    listener.listen(16)
    while True:
        connection, _ = listener.accept()
        with connection:
            connection.sendall(b"ok\n")


class DnsFixtureTest(unittest.TestCase):
    def query(self, name, kind=1):
        encoded = b"".join(bytes([len(label)]) + label for label in name.split(b"."))
        return struct.pack("!6H", 713, 0x0100, 1, 0, 0, 0) + encoded + b"\0" + struct.pack(
            "!2H", kind, 1
        )

    def test_native_a_question_retains_identity_and_returns_the_live_control(self):
        query = self.query(b"pool-public.invalid")
        reply = dns_reply(query)
        self.assertEqual(struct.unpack("!6H", reply[:12]), (713, 0x8180, 1, 1, 0, 0))
        self.assertEqual(reply[12 : len(query)], query[12:])
        self.assertEqual(reply[-4:], socket.inet_aton("93.184.216.34"))

    def test_private_answer_is_available_for_the_native_connect_denial_control(self):
        reply = dns_reply(self.query(b"pool-private.invalid"))
        self.assertEqual(reply[-4:], socket.inet_aton("127.0.0.1"))

    def test_ipv6_question_is_nodata_not_a_fabricated_ipv6_route(self):
        reply = dns_reply(self.query(b"pool-public.invalid", 28))
        self.assertEqual(struct.unpack("!6H", reply[:12]), (713, 0x8180, 1, 0, 0, 0))

    def test_unknown_names_are_nxdomain_not_a_general_dns_forwarder(self):
        reply = dns_reply(self.query(b"unrelated.invalid"))
        self.assertEqual(struct.unpack("!6H", reply[:12]), (713, 0x8183, 1, 0, 0, 0))

    def test_truncated_and_response_packets_are_not_reflected(self):
        query = self.query(b"pool-public.invalid")
        for packet in (b"", query[:-1], query[:12] + b"\xc0\x0c", b"\0" * 12):
            self.assertIsNone(dns_reply(packet))
        response = bytearray(query)
        response[2] |= 0x80
        self.assertIsNone(dns_reply(response))


if __name__ == "__main__":
    if sys.argv[1:] == ["--test"]:
        unittest.main(argv=[sys.argv[0]])
    elif sys.argv[1:]:
        raise SystemExit("unexpected egress fixture arguments")
    else:
        threading.Thread(target=serve_dns, daemon=True).start()
        threading.Thread(target=serve, args=("::1", socket.AF_INET6), daemon=True).start()
        serve("0.0.0.0", socket.AF_INET)
