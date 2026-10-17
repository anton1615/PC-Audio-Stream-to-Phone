// Adaptive jitter buffer watermark controller.
//
// This is pure logic with no Android or Oboe dependencies, which is what lets
// it be tested on the build host (see android/app/src/test/cpp).
//
// Occupancy alone cannot decide the right watermark: too low and every Wi-Fi
// hiccup becomes an audible gap, too high and every packet pays the latency
// forever. So the watermark moves in both directions, driven by observed
// underruns on the way up and by a sustained clean link on the way down.
//
// States:
//   Normal      steady state; the configured watermark is in force
//   PreHolding  an underrun was seen; wait to confirm the link is really bad
//               before paying for extra latency
//   Holding     pre-buffering up to the raised watermark
//   PreEmptying the link recovered; draining the watermark back down
//   Emptying    actively discarding backlog that exceeds the watermark
//
// Confirm/release windows keep a single stray underrun from ratcheting the
// latency up permanently, which is what a plain "is buffering" boolean does.

#pragma once

#include <chrono>
#include <cstddef>
#include <functional>

enum class QueueState {
    Normal,
    PreHolding,
    Holding,
    PreEmptying,
    Emptying,
};

// One Opus frame is 20 ms. Buffer sizes are expressed in frames so they can be
// compared with the watermarks without any unit conversion.
constexpr int MS_PER_FRAME = 20;

class AdaptiveBuffer {
public:
    // Frames to add when the link proves unstable. Removal is one step: a
    // confirmed clean window drops the watermark straight back to the base.
    static constexpr int RAISE_STEP_FRAMES = 2;   // +40 ms

    // How long an underrun-free stretch must last before changing state.
    static constexpr int HOLD_CONFIRM_MS = 500;
    static constexpr int HOLD_RELEASE_MS = 3000;
    static constexpr int DRAIN_CONFIRM_MS = 2000;

    // Upper bound growth, from the competitor analysis: once the measured
    // buffered latency gets within 20 ms of the ceiling, the ceiling moves up
    // by 30 ms instead of letting the link underrun.
    static constexpr int LATENCY_DIFF_THRESHOLD_MS = 20;
    static constexpr int DIFF_TARGET_MS = 30;
    static constexpr int MAX_CEILING_FRAMES = 25; // hard cap, 500 ms

    // Room the watermark is allowed to grow into before the ceiling itself has
    // to move. Without this the ceiling would equal the floor and a confirmed
    // underrun could never raise the target at all.
    static constexpr int DEFAULT_HEADROOM_FRAMES = 10; // +200 ms

    using Clock = std::chrono::steady_clock;

    /// Log sink. The header stays free of Android headers so it can be built on
    /// the host for tests; native-lib.cpp installs a sink that forwards to
    /// __android_log_print. Left unset, logging is a no-op.
    using LogSink = std::function<void(const char* fmt, int a, int b, int c, int d)>;

    AdaptiveBuffer() : mNow([] { return Clock::now(); }) {}

    /// Installs the log sink. Call once after construction.
    void setLogSink(LogSink sink) { mLog = std::move(sink); }

    /// Overrides the time source. Only used by tests.
    void setClockForTest(std::function<Clock::time_point()> now) {
        mNow = std::move(now);
    }

    /// Advances the injected clock. Only used by tests.
    void advanceTimeForTest(int ms) {
        mOffsetMs += ms;
    }

    void configure(int baseFrames) {
        baseFrames = clamp(baseFrames, 1, MAX_CEILING_FRAMES);
        mBaseFrames = baseFrames;
        // The ceiling starts above the base so a confirmed underrun has room to
        // raise the watermark before the ceiling itself needs to move.
        mCeilingFrames = clamp(baseFrames + DEFAULT_HEADROOM_FRAMES, baseFrames, MAX_CEILING_FRAMES);
        mTargetFrames = baseFrames;
        mState = QueueState::Normal;
        mStateEnteredAt = now();
        mUnderrunsInState = 0;
    }

    void setPcmTarget(int frames) { mPcmTargetFrames = clamp(frames, 1, 10); }
    void setCatchUp(int frames) { mCatchUpFrames = clamp(frames, 1, 10); }

    int targetFrames() const { return mTargetFrames; }
    int pcmTargetFrames() const { return mPcmTargetFrames; }
    int catchUpFrames() const { return mCatchUpFrames; }
    int ceilingFrames() const { return mCeilingFrames; }
    QueueState state() const { return mState; }

    /// True when the caller should discard backlog beyond the watermark.
    bool shouldDiscard(std::size_t bufferedFrames) const {
        return bufferedFrames > static_cast<std::size_t>(mTargetFrames + mCatchUpFrames);
    }

    /// Called when the audio callback ran out of samples.
    void onUnderrun() {
        mUnderrunsInState++;
        switch (mState) {
            case QueueState::Normal:
                transition(QueueState::PreHolding);
                break;
            case QueueState::PreHolding:
                // Confirmed: the link is not just having a moment.
                mTargetFrames = clamp(mTargetFrames + RAISE_STEP_FRAMES, mBaseFrames, mCeilingFrames);
                log("[Jitter] -> Holding, watermark now %d frames (%d ms)", mTargetFrames, mTargetFrames * MS_PER_FRAME, 0);
                transition(QueueState::Holding);
                break;
            case QueueState::Holding:
                // Still starving even while holding a raised watermark: the
                // ceiling itself is too low, so lift it and grow the target with
                // it. The periodic onTick path cannot help here because it only
                // reacts to occupancy approaching the ceiling, and by definition
                // the buffer is empty at this point.
                if (mCeilingFrames < MAX_CEILING_FRAMES) {
                    int oldCeiling = mCeilingFrames;
                    mCeilingFrames = clamp(mCeilingFrames + (DIFF_TARGET_MS / MS_PER_FRAME),
                                           mBaseFrames, MAX_CEILING_FRAMES);
                    mTargetFrames = clamp(mTargetFrames + RAISE_STEP_FRAMES, mBaseFrames, mCeilingFrames);
                    log("[Jitter] ceiling raised %d -> %d frames on underrun (target %d)",
                        oldCeiling, mCeilingFrames, mTargetFrames);
                }
                break;
            case QueueState::PreEmptying:
            case QueueState::Emptying:
                // Dropping the watermark was premature; put it back.
                mTargetFrames = clamp(mTargetFrames + RAISE_STEP_FRAMES, mBaseFrames, mCeilingFrames);
                log("[Jitter] -> Holding (recovered), watermark %d frames", mTargetFrames, 0, 0);
                transition(QueueState::Holding);
                break;
        }
    }

    /// Called periodically with the current occupancy.
    void onTick(std::size_t bufferedFrames) {
        auto nowTs = now();
        auto elapsedMs = std::chrono::duration_cast<std::chrono::milliseconds>(nowTs - mStateEnteredAt).count();

        // If the buffer is already sitting close to the ceiling, the ceiling
        // itself is the constraint and the next hiccup will underrun. Move it up
        // by a fixed margin above what we are actually measuring, rather than
        // waiting for the underrun to prove the point.
        //
        // Setting it relative to `bufferedFrames` (not incrementing the old
        // ceiling) is what keeps this self-limiting: once the ceiling is a
        // margin above the real occupancy, the condition stops holding and the
        // ceiling stops growing.
        if (mCeilingFrames < MAX_CEILING_FRAMES) {
            int bufferedMs = static_cast<int>(bufferedFrames) * MS_PER_FRAME;
            int ceilingMs = mCeilingFrames * MS_PER_FRAME;
            if (bufferedMs >= ceilingMs - LATENCY_DIFF_THRESHOLD_MS) {
                int raised = static_cast<int>(bufferedFrames) + (DIFF_TARGET_MS / MS_PER_FRAME);
                raised = clamp(raised, mCeilingFrames, MAX_CEILING_FRAMES);
                if (raised > mCeilingFrames) {
                    log("[Jitter] ceiling raised %d -> %d frames (%d ms, buffered %d ms)",
                        mCeilingFrames, raised, raised * MS_PER_FRAME, bufferedMs);
                    mCeilingFrames = raised;
                }
            }
        }

        switch (mState) {
            case QueueState::Normal:
                break;

            case QueueState::PreHolding:
                if (elapsedMs >= HOLD_CONFIRM_MS && mUnderrunsInState == 0) {
                    transition(QueueState::Normal);
                }
                break;

            case QueueState::Holding:
                if (bufferedFrames >= static_cast<std::size_t>(mTargetFrames) && elapsedMs >= HOLD_RELEASE_MS) {
                    log("[Jitter] -> PreEmptying after %d ms stable", static_cast<int>(elapsedMs), 0, 0);
                    transition(QueueState::PreEmptying);
                }
                break;

            case QueueState::PreEmptying:
                if (elapsedMs >= DRAIN_CONFIRM_MS && mUnderrunsInState == 0) {
                    // The link has been clean for the whole confirm window, so
                    // pay the base latency again in one step. Draining a frame
                    // at a time kept sessions pinned at the raised watermark
                    // for half a minute after every transient underrun burst.
                    if (mTargetFrames > mBaseFrames) {
                        log("[Jitter] -> Normal, watermark %d -> %d frames (stable %d ms)",
                            mTargetFrames, mBaseFrames, static_cast<int>(elapsedMs));
                        mTargetFrames = mBaseFrames;
                    }
                    transition(QueueState::Normal);
                }
                break;

            case QueueState::Emptying:
                if (bufferedFrames <= static_cast<std::size_t>(mTargetFrames)) {
                    transition(QueueState::PreEmptying);
                }
                break;
        }
    }

    /// Called when the caller actually discarded backlog.
    void onDiscarded() {
        if (mState != QueueState::Emptying) {
            transition(QueueState::Emptying);
        }
    }

private:
    static int clamp(int v, int lo, int hi) {
        return v < lo ? lo : (v > hi ? hi : v);
    }

    void log(const char* fmt, int a, int b, int c, int d = 0) const {
        if (mLog) mLog(fmt, a, b, c, d);
    }

    Clock::time_point now() const {
        return mNow() + std::chrono::milliseconds(mOffsetMs);
    }

    void transition(QueueState next) {
        mState = next;
        mStateEnteredAt = now();
        mUnderrunsInState = 0;
    }

    std::function<Clock::time_point()> mNow;
    LogSink mLog;
    int mOffsetMs = 0;
    int mBaseFrames = 5;
    int mTargetFrames = 5;
    int mCeilingFrames = 5;
    int mPcmTargetFrames = 5;
    int mCatchUpFrames = 1;
    QueueState mState = QueueState::Normal;
    Clock::time_point mStateEnteredAt = Clock::now();
    int mUnderrunsInState = 0;
};
