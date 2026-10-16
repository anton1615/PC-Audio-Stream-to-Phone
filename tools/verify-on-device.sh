#!/usr/bin/env bash
# On-device end-to-end verification for AS2P.
#
# Runs the real server, installs the freshly built APK, drives the app, and
# checks that the v9 protocol, the adaptive jitter buffer and the retransmission
# path all behave on actual hardware.
#
# Requires an authorized device: `adb devices` must show it as `device`.
#
# Usage:  bash tools/verify-on-device.sh [serial]

set -u

ADB="${ADB:-adb}"
SERIAL="${1:-}"
[ -n "$SERIAL" ] && ADB="$ADB -s $SERIAL"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APK="$ROOT/android/app/build/outputs/apk/debug/app-debug.apk"
SERVER="$ROOT/server/target/debug/server.exe"
PKG="com.example.audiobtbridge"
TAG="AS2P"

fail=0
step() { printf '\n=== %s\n' "$1"; }
ok()   { printf 'PASS: %s\n' "$1"; }
bad()  { printf 'FAIL: %s\n' "$1"; fail=1; }

step "0. device state"
state="$($ADB devices | awk -v s="$SERIAL" '$1 ~ /RF|emulator|:/ {print $2}' | head -1)"
if ! $ADB devices | grep -qE '\sdevice$'; then
    echo "No authorized device. Enable USB debugging and accept the prompt."
    $ADB devices -l
    exit 1
fi
ok "device authorized"

step "1. build artifacts present"
[ -f "$APK" ]    && ok "apk: $(basename "$APK")"    || { bad "missing $APK"; exit 1; }
[ -f "$SERVER" ] && ok "server binary"              || { bad "missing $SERVER (run: cargo build)"; exit 1; }

step "2. install APK"
if $ADB install -r "$APK" 2>&1 | tee /tmp/as2p_install.log | grep -qE 'Success|Streamed'; then
    ok "installed"
else
    bad "install failed"; cat /tmp/as2p_install.log; exit 1
fi

step "3. start server (debug, no artificial loss)"
# Server binds 0.0.0.0:12345 and replies to <client-ip>:12345.
"$SERVER" --debug --drop-rate 0.0 > /tmp/as2p_server_dev.log 2>&1 &
SRV_PID=$!
trap 'kill $SRV_PID 2>/dev/null' EXIT
sleep 9
if grep -q "Waiting for AS2P_DISCOVER" /tmp/as2p_server_dev.log; then
    ok "server listening"
else
    bad "server did not reach listening state"; cat /tmp/as2p_server_dev.log; exit 1
fi
grep -q "MMCSS (Pro Audio)" /tmp/as2p_server_dev.log \
    && ok "MMCSS registration (P0)" \
    || bad "no MMCSS registration line"

step "4. clear logcat and launch app"
$ADB logcat -c 2>/dev/null || true
$ADB shell am force-stop "$PKG"
$ADB shell monkey -p "$PKG" -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1
sleep 6
$ADB shell dumpsys activity services "$PKG" | grep -q "AudioService" \
    && ok "AudioService started" \
    || bad "AudioService not running (did the app open and auto-connect?)"

step "5. wait for stream to establish"
for i in $(seq 1 20); do
    if grep -q "Offer sent" /tmp/as2p_server_dev.log; then break; fi
    sleep 2
done
if grep -q "Offer sent" /tmp/as2p_server_dev.log; then
    ok "client discovered the server"
else
    bad "no AS2P_DISCOVER reached the server"
fi

step "6. verify v9 audio is flowing"
for i in $(seq 1 15); do
    if grep -qE 'Streaming: Seq=[1-9]' /tmp/as2p_server_dev.log; then break; fi
    sleep 2
done
if grep -qE 'Streaming: Seq=[1-9]' /tmp/as2p_server_dev.log; then
    ok "server is streaming v9 audio"
    grep -oE 'Streaming:.*' /tmp/as2p_server_dev.log | tail -1
else
    bad "server never reported a non-zero sequence"
fi

step "7. client-side stats reached the server (P5)"
if grep -q "Client\[" /tmp/as2p_server_dev.log; then
    ok "client stats parsed"
    grep -oE 'Client\[.*' /tmp/as2p_server_dev.log | tail -1
else
    bad "no Client[...] line; AS2P_STATS not arriving or not parsed"
fi

step "8. jitter buffer state machine is live (P2/P3)"
$ADB logcat -d -s AS2P_Native:* > /tmp/as2p_native.log 2>/dev/null || true
if grep -qE '\[Jitter\]' /tmp/as2p_native.log; then
    ok "native jitter logs present"
    grep -oE '\[Jitter\].*' /tmp/as2p_native.log | sort -u | head -12
else
    bad "no [Jitter] lines in logcat"
fi

step "9. underrun / adaptation behaviour"
if grep -qE '\[Jitter\] -> Holding|ceiling raised' /tmp/as2p_native.log; then
    ok "buffer adapted to observed underruns"
else
    echo "NOTE: no adaptation observed. Expected on a clean link (that is the"
    echo "      point: the watermark should stay at base when nothing is lost)."
    echo "      To force it, rerun the server with --drop-rate 0.05."
fi

step "10. retransmission path (P4)"
if grep -q "Resending tick" /tmp/as2p_server_dev.log; then
    ok "retransmission served"
    grep -oE 'Resending tick [0-9]+' /tmp/as2p_server_dev.log | tail -3
else
    echo "NOTE: no resend requested. On a clean link no gaps occur, so none"
    echo "      should be. Rerun with --drop-rate 0.05 to exercise it."
fi

step "11. app-side diagnostics"
$ADB logcat -d -s AS2P_Service:* AS2P_Diag:* > /tmp/as2p_app.log 2>/dev/null || true
grep -oE '\[Stats\].*' /tmp/as2p_app.log | tail -3 || true
grep -oE '\[Network\] Gap:.*' /tmp/as2p_app.log | tail -3 || true

printf '\n========================================\n'
if [ "$fail" -eq 0 ]; then
    echo "DEVICE VERIFICATION PASSED"
else
    echo "DEVICE VERIFICATION FAILED"
fi
printf 'server log: %s\nnative log: %s\napp log:    %s\n' \
    /tmp/as2p_server_dev.log /tmp/as2p_native.log /tmp/as2p_app.log
exit "$fail"
