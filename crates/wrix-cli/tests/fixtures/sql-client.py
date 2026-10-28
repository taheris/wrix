"""External-runtime transport adapter: exchange bytes with the actual owned SQL gate."""
import os
import json
import socket
import sys

request = {"command": "abort"}
if len(sys.argv) > 2:
    if sys.argv[2] == "gateway":
        args = sys.argv[3:]
        ports = {int(args[i+1].rsplit(":", 1)[1]): args[i+1].rsplit(":", 1)[0] for i, arg in enumerate(args) if arg == "-p"}
        request = {"role": "gateway", "data": ports[3128], "management": ports[3129], "key": os.environ["WRIX_GATEWAY_KEY"]}
    elif sys.argv[2] == "compiler-ready":
        request = {"role": "compiler-ready"}
    elif sys.argv[2] == "forward":
        request = {"role": "forward", "listen": sys.argv[3], "target": sys.argv[4]}
    else:
        raise SystemExit("unsupported native role request")
elif len(sys.argv) != 2:
    raise SystemExit("expected the fixture-owned SQL control socket")
with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
    client.settimeout(30)
    client.connect(sys.argv[1])
    client.sendall(json.dumps(request).encode() + b"\n")
    response = bytearray()
    while not response.endswith(b'\n'):
        chunk = client.recv(4097 - len(response))
        if not chunk or len(response) + len(chunk) > 4096:
            raise SystemExit("invalid native SQL control frame")
        response.extend(chunk)
    reply = json.loads(response)
    if "error" in reply:
        raise SystemExit(reply["error"])
    sys.stdout.buffer.write(response)
