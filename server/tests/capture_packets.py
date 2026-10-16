"""Captures real v9 audio packets from the AS2P server.

Used to feed the Android receive path on an emulator, where the server's
symmetric fixed-port reply cannot route back through SLIRP user-mode NAT.

Writes length-prefixed packets (u32 LE length + bytes) so the receiver can split
them without parsing the protocol.

Usage: python tests/capture_packets.py <out.bin> [count]
"""

import os
import socket
import struct
import sys
import time

PREFIX = b"AS2P_AUDIO"


def main():
    out_path = sys.argv[1] if len(sys.argv) > 1 else "packets.bin"
    want = int(sys.argv[2]) if len(sys.argv) > 2 else 60
    port = 12345
    client_ip = "127.0.0.2"
    server_ip = "127.0.0.1"

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind((client_ip, port))
    sock.settimeout(3.0)

    sock.sendto(b"AS2P_DISCOVER", (server_ip, port))
    deadline = time.time() + 5
    while time.time() < deadline:
        try:
            data, _ = sock.recvfrom(2048)
        except socket.timeout:
            break
        if data.startswith(b"AS2P_OFFER"):
            break

    sock.sendto(b"AS2P_CONFIG" + struct.pack("<iB", 160000, 5), (server_ip, port))

    packets = []
    deadline = time.time() + 15
    while time.time() < deadline and len(packets) < want:
        try:
            data, _ = sock.recvfrom(2048)
        except socket.timeout:
            continue
        if data.startswith(PREFIX):
            packets.append(data)

    sock.close()

    out_dir = os.path.dirname(out_path)
    if out_dir:
        os.makedirs(out_dir, exist_ok=True)
    with open(out_path, "wb") as fh:
        for p in packets:
            fh.write(struct.pack("<I", len(p)))
            fh.write(p)

    # Optional: one file per packet, so a guest-side replay needs no parsing
    # (toybox has no length-prefix-aware tooling).
    split_dir = sys.argv[3] if len(sys.argv) > 3 else None
    if split_dir:
        os.makedirs(split_dir, exist_ok=True)
        for i, p in enumerate(packets):
            with open(os.path.join(split_dir, f"pkt_{i:04d}.bin"), "wb") as fh:
                fh.write(p)
        print(f"split {len(packets)} packets -> {split_dir}")

    sizes = sorted({len(p) for p in packets})
    print(f"captured {len(packets)} packets -> {out_path}")
    print(f"distinct sizes: {sizes}")
    if packets:
        tick, ts, flags, seq = struct.unpack_from("<QQQI", packets[0], len(PREFIX))
        print(f"first: tick={tick} flags={flags} seq={seq} payload={len(packets[0]) - len(PREFIX) - 28}B")
    return 0 if packets else 1


if __name__ == "__main__":
    sys.exit(main())
