"""End-to-end smoke test for the AS2P v9 protocol against the real server binary.

Verifies the parts of the protocol that are observable on the wire:
  - discovery handshake (AS2P_DISCOVER -> AS2P_OFFER)
  - v9 audio header layout and media tick monotonicity
  - retransmission (AS2P_RESEND -> packet with FLAG_RETRANSMIT)
  - client stats reporting (AS2P_STATS is accepted without disturbing the stream)
  - real-time supply rate (the client consumes 50 frames/s; a slow fallback
    starves its jitter buffer when the PC is silent)
"""

import socket
import struct
import sys
import time

PREFIX = b"AS2P_AUDIO"
HEADER_LEN = 28  # tick(8) + send_ts(8) + flags(8) + seq(4), after the prefix
FLAG_RETRANSMIT = 1

def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 12345
    # AS2P is symmetric: the server always replies to <source-ip>:12345, not to
    # the sender's ephemeral port. The real Android client binds 12345 for the
    # same reason. On loopback that means the client needs its own address, so
    # it uses the 127.0.0.2 alias rather than colliding with the server on
    # 127.0.0.1.
    client_ip = sys.argv[2] if len(sys.argv) > 2 else "127.0.0.2"
    server_ip = "127.0.0.1"

    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind((client_ip, port))
    sock.settimeout(3.0)

    failures = []

    # --- 1. discovery ---
    sock.sendto(b"AS2P_DISCOVER", (server_ip, port))
    offer = None
    deadline = time.time() + 5
    while time.time() < deadline:
        try:
            data, addr = sock.recvfrom(2048)
        except socket.timeout:
            break
        if data.startswith(b"AS2P_OFFER"):
            offer = (data, addr)
            break
    if not offer:
        print("FAIL: no AS2P_OFFER received")
        return 1
    print(f"PASS: AS2P_OFFER from {offer[1]}")

    # --- 2. config -> start stream ---
    cfg = b"AS2P_CONFIG" + struct.pack("<iB", 160000, 5)
    sock.sendto(cfg, (server_ip, port))

    # --- 3. collect v9 audio packets ---
    packets = []
    deadline = time.time() + 6
    while time.time() < deadline and len(packets) < 40:
        try:
            data, _ = sock.recvfrom(2048)
        except socket.timeout:
            continue
        if data.startswith(PREFIX) and len(data) >= len(PREFIX) + HEADER_LEN:
            tick, send_ts, flags, seq = struct.unpack_from("<QQQI", data, len(PREFIX))
            payload = data[len(PREFIX) + HEADER_LEN:]
            packets.append((tick, send_ts, flags, seq, len(payload)))

    if len(packets) < 10:
        failures.append(f"only {len(packets)} audio packets received (expected >=10)")
    else:
        print(f"PASS: received {len(packets)} v9 audio packets")

    if packets:
        ticks = [p[0] for p in packets if p[2] & FLAG_RETRANSMIT == 0]
        seqs = [p[3] for p in packets if p[2] & FLAG_RETRANSMIT == 0]
        if ticks != sorted(ticks):
            failures.append("media ticks are not monotonic")
        else:
            print(f"PASS: media ticks monotonic ({ticks[0]}..{ticks[-1]})")
        if seqs != sorted(seqs):
            failures.append("seq values are not monotonic")
        else:
            print(f"PASS: seq monotonic ({seqs[0]}..{seqs[-1]})")
        empty = [p for p in packets if p[4] == 0]
        if empty:
            failures.append(f"{len(empty)} packets carried an empty payload")
        else:
            print(f"PASS: all payloads non-empty (first={packets[0][4]}B)")

    # --- 4. retransmission ---
    if packets:
        # Ask for a tick that is still inside the 128-packet cache window.
        target_tick = packets[len(packets) // 2][0]
        sock.sendto(b"AS2P_RESEND" + struct.pack("<Q", target_tick), (server_ip, port))
        got_resend = False
        deadline = time.time() + 3
        while time.time() < deadline:
            try:
                data, _ = sock.recvfrom(2048)
            except socket.timeout:
                break
            if data.startswith(PREFIX) and len(data) >= len(PREFIX) + HEADER_LEN:
                tick, _, flags, _ = struct.unpack_from("<QQQI", data, len(PREFIX))
                if (flags & FLAG_RETRANSMIT) and tick == target_tick:
                    got_resend = True
                    break
        if got_resend:
            print(f"PASS: retransmission served for tick {target_tick}")
        else:
            failures.append(f"no retransmission for tick {target_tick}")

    # --- 5. stats packet is accepted ---
    stats = b"AS2P_STATS" + struct.pack("<5I", 100, 80, 1234, 56, 7)
    sock.sendto(stats, (server_ip, port))
    time.sleep(0.5)
    # Stream must still be alive after the stats packet.
    still_alive = False
    deadline = time.time() + 2
    while time.time() < deadline:
        try:
            data, _ = sock.recvfrom(2048)
        except socket.timeout:
            break
        if data.startswith(PREFIX):
            still_alive = True
            break
    if still_alive:
        print("PASS: stream alive after AS2P_STATS")
    else:
        failures.append("stream stopped after AS2P_STATS")

    # --- 6. supply rate ---
    # The client consumes 50 frames/s. When the PC is fully silent the server
    # must still supply real time from its silence fallback; measuring below
    # this rate starved the client's jitter buffer and ratcheted the adaptive
    # watermark to its ceiling (up to 500 ms of latency after audio resumed).
    # Bound covers both regimes: real WASAPI callbacks and the fallback.
    rate_packets = []
    rate_deadline = time.time() + 3
    while time.time() < rate_deadline:
        try:
            data, _ = sock.recvfrom(2048)
        except socket.timeout:
            continue
        if data.startswith(PREFIX) and len(data) >= len(PREFIX) + HEADER_LEN:
            tick, send_ts, flags, _ = struct.unpack_from("<QQQI", data, len(PREFIX))
            if not (flags & FLAG_RETRANSMIT):
                rate_packets.append((tick, send_ts))
    if len(rate_packets) >= 20:
        span_s = (rate_packets[-1][1] - rate_packets[0][1]) / 1000.0
        rate = (rate_packets[-1][0] - rate_packets[0][0]) / span_s
        if 45.0 <= rate <= 55.0:
            print(f"PASS: supply rate {rate:.1f} frames/s")
        else:
            failures.append(f"supply rate {rate:.1f} frames/s outside 45-55")
    else:
        failures.append(f"only {len(rate_packets)} packets for rate check (expected >=20)")
    sock.close()

    print()
    if failures:
        for f in failures:
            print("FAIL:", f)
        return 1
    print("ALL CHECKS PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
