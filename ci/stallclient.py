"""Connect, complete the WebSocket handshake, then stop reading — forever.

The adversarial viewer: a paused browser tab. Its kernel receive buffer fills, the server's
write_all blocks, and before the write timeout the whole producer froze behind it.
"""
import base64
import os
import socket
import sys
import time

host, port, hold = sys.argv[1], int(sys.argv[2]), float(sys.argv[3])
s = socket.create_connection((host, port))
key = base64.b64encode(os.urandom(16)).decode()
s.sendall(
    f"GET / HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\n"
    f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
    f"Sec-WebSocket-Version: 13\r\n\r\n".encode()
)
# Read exactly the handshake response, then never read again.
buf = b""
while b"\r\n\r\n" not in buf:
    b = s.recv(1)
    if not b:
        break
    buf += b
print("stalled client: handshake done, now reading nothing", flush=True)
time.sleep(hold)
s.close()
