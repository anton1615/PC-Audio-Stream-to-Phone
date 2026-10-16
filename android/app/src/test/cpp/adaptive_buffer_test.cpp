// Host-side test for AdaptiveBuffer, the jitter buffer's watermark controller.
//
// The controller is pure logic (no JNI, no Oboe), so it is exercised here on the
// build host instead of only on a device. Thresholds are in the same units the
// real code uses: frames of 20 ms.
//
// Build & run:
//   c++ -std=c++17 android/app/src/test/cpp/adaptive_buffer_test.cpp -o /tmp/ab_test && /tmp/ab_test

#include <cstdio>

#include "adaptive_buffer.h"

static int failures = 0;

#define CHECK(cond, msg)                                                 \
    do {                                                                 \
        if (!(cond)) {                                                   \
            std::printf("FAIL: %s (%s:%d)\n", msg, __FILE__, __LINE__);  \
            failures++;                                                  \
        }                                                                \
    } while (0)

// A single underrun must not permanently raise the watermark: once the confirm
// window passes with no further underruns, the controller returns to Normal.
static void test_single_underrun_is_transient() {
    AdaptiveBuffer b;
    b.configure(5);
    CHECK(b.state() == QueueState::Normal, "starts Normal");
    CHECK(b.targetFrames() == 5, "starts at base");

    b.onUnderrun();
    CHECK(b.state() == QueueState::PreHolding, "underrun -> PreHolding");

    b.advanceTimeForTest(600);   // past HOLD_CONFIRM_MS
    b.onTick(5);
    CHECK(b.state() == QueueState::Normal, "recovers to Normal");
    CHECK(b.targetFrames() == 5, "watermark unchanged after a lone underrun");
}

// A confirmed bad link must raise the watermark, then drain it back down.
static void test_confirmed_underruns_raise_then_drain() {
    AdaptiveBuffer b;
    b.configure(5);

    b.onUnderrun();   // Normal -> PreHolding
    b.onUnderrun();   // PreHolding -> Holding, +2 frames
    CHECK(b.state() == QueueState::Holding, "two underruns -> Holding");
    CHECK(b.targetFrames() == 7, "watermark raised by RAISE_STEP_FRAMES");

    b.advanceTimeForTest(3100);   // past HOLD_RELEASE_MS
    b.onTick(20);                 // comfortably buffered
    CHECK(b.state() == QueueState::PreEmptying, "stable -> PreEmptying");

    b.advanceTimeForTest(2100);   // past DRAIN_CONFIRM_MS
    b.onTick(20);
    CHECK(b.targetFrames() == 6, "drains one step at a time");

    b.advanceTimeForTest(2100);
    b.onTick(20);
    CHECK(b.targetFrames() == 5, "drains back to base");
}

// The ceiling must rise when occupancy approaches it, and must stop rising once
// it sits a margin above the measured occupancy (otherwise it ratchets forever).
static void test_ceiling_is_self_limiting() {
    AdaptiveBuffer b;
    b.configure(5);
    int initialCeiling = b.ceilingFrames();
    CHECK(initialCeiling > 5, "ceiling starts above the base (headroom)");

    // Drive occupancy right up to the ceiling so it has to move.
    for (int i = 0; i < 50; i++) {
        b.onTick(initialCeiling);
    }
    int after = b.ceilingFrames();
    CHECK(after > initialCeiling, "ceiling rose when occupancy reached it");

    for (int i = 0; i < 50; i++) {
        b.onTick(2);   // occupancy well below the raised ceiling
    }
    CHECK(b.ceilingFrames() == after, "ceiling stops growing once above occupancy");
}

// The watermark must never exceed the ceiling, and the ceiling never exceeds the cap.
static void test_bounds_are_respected() {
    AdaptiveBuffer b;
    b.configure(5);

    // Hammer the Holding path with underruns; both bounds must hold.
    b.onUnderrun();
    for (int i = 0; i < 200; i++) {
        b.onUnderrun();
    }
    CHECK(b.targetFrames() <= b.ceilingFrames(), "target never exceeds ceiling");
    CHECK(b.ceilingFrames() <= AdaptiveBuffer::MAX_CEILING_FRAMES, "ceiling respects the hard cap");
    CHECK(b.targetFrames() >= 5, "target never drops below base");
}

// Discarding backlog must be reported so the caller can trim the queue.
static void test_shouldDiscard() {
    AdaptiveBuffer b;
    b.configure(5);
    b.setCatchUp(1);

    CHECK(!b.shouldDiscard(5), "at watermark: keep");
    CHECK(!b.shouldDiscard(6), "at watermark+catchup: keep");
    CHECK(b.shouldDiscard(7), "above watermark+catchup: discard");
}

// Configuring must fully reset the controller.
static void test_configure_resets() {
    AdaptiveBuffer b;
    b.configure(5);
    b.onUnderrun();
    b.onUnderrun();
    CHECK(b.targetFrames() != 5, "raised before reconfigure");

    b.configure(8);
    CHECK(b.targetFrames() == 8, "target reset to new base");
    CHECK(b.ceilingFrames() > 8, "ceiling reset with headroom above new base");
    CHECK(b.state() == QueueState::Normal, "state reset to Normal");
}

int main() {
    test_single_underrun_is_transient();
    test_confirmed_underruns_raise_then_drain();
    test_ceiling_is_self_limiting();
    test_bounds_are_respected();
    test_shouldDiscard();
    test_configure_resets();

    if (failures == 0) {
        std::printf("ALL ADAPTIVE BUFFER TESTS PASSED\n");
        return 0;
    }
    std::printf("%d FAILURE(S)\n", failures);
    return 1;
}
