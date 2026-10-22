# AGENTS.md

Guidance for agents working in this repository.

## Project layout

| Path | What it is |
|---|---|
| `server/` | Rust. Windows capture (cpal/WASAPI loopback) + Opus encode + UDP send, with a Slint tray UI. |
| `android/` | Kotlin app + C++ native audio engine (`app/src/main/cpp`). Oboe output, Opus decode, jitter buffer. |
| `docs/` | Analysis notes. `competitor-analysis.md` records what was learned from a competing implementation and which parts were adopted. |
| `tools/` | Helper scripts. `verify-on-device.sh` runs the on-device end-to-end check. |

## Build and test

```bash
# Server: build, unit tests, end-to-end protocol check
cd server
cargo test
cargo build
python tests/protocol_smoke.py 12345     # needs a running server

# Android: full APK (native + Kotlin)
cd android
java -cp gradle/wrapper/gradle-wrapper.jar org.gradle.wrapper.GradleWrapperMain \
    :app:assembleDebug

# Jitter buffer controller: pure logic, testable on the host
g++ -std=c++17 -I android/app/src/main/cpp \
    android/app/src/test/cpp/adaptive_buffer_test.cpp -o /tmp/ab_test && /tmp/ab_test
```

The Gradle wrapper is invoked through `java -cp` rather than `./gradlew` because
the checked-in `gradlew` script does not resolve its own wrapper jar on Windows
(Git Bash). CI uses `gradle assembleDebug` from `gradle/actions/setup-gradle`.

The Android build needs `android/local.properties` pointing at the SDK, and
`NDK 25.1.8937393` installed. Oboe and Opus are fetched by CMake on first build.
On a fresh machine the Gradle distribution download can stall; pre-seeding
`~/.gradle/wrapper/dists/gradle-8.10.2-bin/` with the distribution zip works
around it.

## Protocol invariants

These are easy to break and expensive to debug. Read before touching the wire
format or the receive path.

**The v9 audio header is 38 bytes** (`AS2P_AUDIO` + 28 bytes of fields):

```
 0..10   "AS2P_AUDIO"
10..18   media_tick    u64   monotonic frame counter, 20 ms per tick
18..26   send_ts_ms    u64   sender wall clock
26..34   flags         u64   bit0 = FLAG_RETRANSMIT
34..38   seq           u32   transmit order
38..     opus payload
```

- **`media_tick` and `send_ts_ms` must stay separate.** Lateness is judged on the
  media timeline, never on arrival time. A retransmitted packet is old by arrival
  time but still exactly on time in the media timeline.
- **The header layout is asserted by `network.rs` tests.** Update
  `audio_packet_layout_is_stable` alongside any format change, and keep
  `PACKET_HEADER_LEN` in `native-lib.cpp` and `AUDIO_HEADER_LEN` in
  `AudioService.kt` in sync with it.
- **`AS2P_AUDIO` packets keep their header all the way to the native layer.**
  Kotlin strips only the 10-byte prefix; the native side parses the tick itself
  because it keys the jitter buffer on it.
- **AS2P is symmetric on port 12345.** The server replies to `<source-ip>:12345`,
  not to the sender's ephemeral port. This breaks under user-mode NAT (emulator
  SLIRP rewrites the source to `127.0.0.1`), so an emulator cannot exercise the
  full round trip. Use a real device on the same LAN.
- **The audio supply must run at real time (50 frames/s).** The client consumes
  one 20 ms frame every 20 ms. When nothing renders on the PC, WASAPI loopback
  stops delivering callbacks and the silence fallback in `audio.rs` becomes the
  sole supply: it emits one 20 ms frame per 20 ms of wall clock on a
  microsecond-credit timeline. A slower fallback starves the client's jitter
  buffer, the underrun storm ratchets the adaptive watermark to its ceiling,
  and audio that resumes afterwards pays hundreds of ms of added latency.
  `tests/protocol_smoke.py` asserts 45-55 frames/s.

## Native audio engine invariants

- **`JNI_OnLoad` must be inside `extern "C"`.** Outside it, C++ mangles the
  symbol, the JVM never finds it, `gJvm` stays null, and every callback that
  needs to attach a native thread to the VM silently stops working. This has
  already shipped broken once. Verify with
  `llvm-readelf --dyn-syms <lib>.so | grep JNI_OnLoad` — the name must be
  unmangled.
- **The decode thread is a plain `std::thread`.** It has no `JNIEnv`; attach
  before calling into Kotlin and detach afterwards.
- **Counters read by more than one loop must be plain reads.** `getPLCCount`,
  `getUnderflowCount` and `getDiscardedFrames` are read by both the periodic log
  line and the stats report. A destructive read (`exchange(0)`) lets whichever
  loop runs first steal the other's samples.
- **Only real starvation may raise the watermark.** The audio callback plays
  from `mPcmBuffer` only; the decode thread refills it from the jitter buffer.
  PCM dry with the jitter buffer still holding frames is decode lag (a
  scheduler hiccup), not link instability — it is counted separately
  (`mDecodeLagCount`) and must not call `AdaptiveBuffer::onUnderrun()` or latch
  `mIsBuffering`. Treating it as an underrun ratcheted sessions to the 500 ms
  ceiling within a minute.
- **`AdaptiveBuffer` is mutated from the audio callback.** Read its fields under
  `mBufferMutex`, never directly.
- **The resend callback is copied under the lock and invoked outside it.** It
  crosses into Kotlin and sends a UDP packet; holding `mBufferMutex` across that
  would stall the audio callback.
- **The jitter buffer controller lives in `adaptive_buffer.h`** so it can be
  tested on the host. Keep it free of Android and Oboe headers; it takes a log
  sink instead of calling `__android_log_print` directly.

## Android service invariants

- **Audio focus loss never stops the stream.** The phone is the speaker: using
  other apps while streaming is the normal case, and any of them can take
  focus (video, game, notification beep). The v9 port adopted the competitor's
  "stop on focus loss" policy for a day; the transient-loss variants of the
  callback killed the stream on a single notification sound. Observe the
  callback, keep the focus request, let audio mix.

## Adaptive buffer behaviour

The watermark is not a fixed value. It moves in both directions:

| State | Enters when | Leaves when |
|---|---|---|
| `Normal` | start / drained back | underrun → `PreHolding` |
| `PreHolding` | underrun seen | 500 ms clean → `Normal`; another underrun → `Holding` |
| `Holding` | instability confirmed | buffered and stable 3000 ms → `PreEmptying` |
| `PreEmptying` | link recovered | 2000 ms clean → drop straight to base (`Normal`) |
| `Emptying` | backlog actually discarded | occupancy back at watermark → `PreEmptying` |

The ceiling starts above the base and grows **relative to measured occupancy**,
not by incrementing itself — that is what keeps it from ratcheting upward
forever. A single stray underrun must not permanently raise latency; that is
what the confirm windows are for.

## Release signing invariants

- **The release signing key is not in this repository.** `android/app/debug.keystore`
  was public from 2026-01-22 and signed every release up to v1.3.3; it is burned.
  v1.3.4+ is signed with a key restored in CI from GitHub Actions secrets.
  Never commit a keystore, a `keystore.properties`, or a base64 blob of one.
- **A release build without signing material must fail.** `android/app/build.gradle`
  throws when a release variant is requested without `keystore.properties` or the
  `AS2P_KEYSTORE_*` environment variables, so an unsigned APK cannot be published.
- **The CI workflow pins the expected certificate SHA-256.** Changing the key means
  updating that constant in `.github/workflows/release.yml` in the same commit.
- **The key must exist in at least two places, one of them offline.** GitHub Actions
  secrets cannot be read back, so they are not a backup. See `RELEASING.md`.

## Conventions

- Comments explain *why*, not *what*. Non-obvious thresholds and wire-format
  decisions get a one-line rationale.
- `shortcut: <limit>, <when to upgrade>` marks a deliberate shortcut.
- The analysis in `docs/competitor-analysis.md` distinguishes what was
  adopted from what was deliberately rejected. When adding a feature that came
  from that analysis, record it there rather than restating it in code comments.
