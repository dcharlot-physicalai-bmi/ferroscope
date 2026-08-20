"""A minimal WebSocket client: handshake, then collect binary frames until the server closes.
Hand-rolled so the CI gate depends on nothing but python3 itself."""
import socket, sys, base64, os, hashlib

host, port, out = sys.argv[1], int(sys.argv[2]), sys.argv[3]
key = base64.b64encode(os.urandom(16)).decode()
s = socket.create_connection((host, port), timeout=30)
s.sendall((f"GET / HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\n"
           f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
           f"Sec-WebSocket-Version: 13\r\n\r\n").encode())
resp = b""
while b"\r\n\r\n" not in resp:
    resp += s.recv(1)
want = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
assert want.encode() in resp, f"bad accept: {resp!r}"

def read_exact(n):
    b = b""
    while len(b) < n:
        chunk = s.recv(n - len(b))
        if not chunk:
            return b
        b += chunk
    return b

frames = 0
data = bytearray()
while True:
    hdr = read_exact(2)
    if len(hdr) < 2:
        break
    fin_op, ln = hdr[0], hdr[1] & 0x7F
    if ln == 126:
        ln = int.from_bytes(read_exact(2), "big")
    elif ln == 127:
        ln = int.from_bytes(read_exact(8), "big")
    payload = read_exact(ln)
    op = fin_op & 0x0F
    if op == 8:  # close
        break
    if op == 2:  # binary
        data.extend(payload)
        frames += 1
open(out, "wb").write(bytes(data))
print(f"{frames} frames, {len(data)} bytes")
