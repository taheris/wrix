import socket
import threading


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


threading.Thread(target=serve, args=("::1", socket.AF_INET6), daemon=True).start()
serve("0.0.0.0", socket.AF_INET)
