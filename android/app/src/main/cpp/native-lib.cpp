#include <jni.h>
#include <string>
#include <functional>
#include <android/log.h>
#include <oboe/Oboe.h>
#include <vector>
#include <map>
#include <mutex>
#include <opus.h>
#include <atomic>
#include <thread>
#include <chrono>
#include <sched.h>
#include <unistd.h>
#include <sys/resource.h>
#include <condition_variable>

#include "adaptive_buffer.h"

#define LOG_TAG "AS2P_Native"
#define LOGI(...) __android_log_print(ANDROID_LOG_INFO, LOG_TAG, __VA_ARGS__)

const int SAMPLE_RATE = 48000;
const int CHANNELS = 2;
const int MAX_FRAME_SIZE = 960;
const int SAMPLES_PER_FRAME = MAX_FRAME_SIZE * CHANNELS;

// Server packet header, after the "AS2P_AUDIO" prefix has been stripped:
//   tick(8) + send_ts_ms(8) + flags(8) + seq(4)
const int PACKET_HEADER_LEN = 28;

// ---------------------------------------------------------------------------
// Adaptive jitter buffer
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
// ---------------------------------------------------------------------------
// QueueState and AdaptiveBuffer live in adaptive_buffer.h so the watermark
// controller can be tested on the build host without Android or Oboe.

class AudioEngine : public oboe::AudioStreamDataCallback, public oboe::AudioStreamErrorCallback {
public:
    AudioEngine() {
        // Forward the controller's state transitions to logcat. The header stays
        // free of Android headers so it can be tested on the host.
        mAdaptive.setLogSink([](const char* fmt, int a, int b, int c, int d) {
            __android_log_print(ANDROID_LOG_INFO, LOG_TAG, fmt, a, b, c, d);
        });
        createDecoder();
    }
    ~AudioEngine() { stop(); destroyDecoder(); }

    void destroyDecoder() {
        if (mOpusDecoder) { opus_decoder_destroy(mOpusDecoder); mOpusDecoder = nullptr; }
    }

    void createDecoder() {
        destroyDecoder();
        int error;
        mOpusDecoder = opus_decoder_create(SAMPLE_RATE, CHANNELS, &error);
    }

    void resetInternal() {
        std::lock_guard<std::mutex> lock(mBufferMutex);
        mJitterBuffer.clear();
        mPcmBuffer.clear();
        mIsBuffering = true;
        mFirstPacket = true;
        mExpectedSeq = 0;
        mPlcCount = 0;
        mUnderflowCount = 0;
        mDiscardedFrames = 0;
        mMaxBufferedFrames = 0;
        mLatePackets = 0;
        mTotalPackets = 0;
        if (mOpusDecoder) opus_decoder_ctl(mOpusDecoder, OPUS_RESET_STATE);
        mDecodeCV.notify_all(); // Wake up worker to acknowledge reset
        LOGI(">>> ENGINE RESET (Atomic & Synced) <<<");
    }

    void start() {
        std::lock_guard<std::mutex> lock(mStreamMutex);
        if (mStream) return;
        resetInternal();

        mIsRunning = true;
        mDecodeThread = std::thread(&AudioEngine::decodeLoop, this);

        oboe::AudioStreamBuilder builder;
        builder.setDirection(oboe::Direction::Output)
               ->setPerformanceMode(oboe::PerformanceMode::LowLatency)
               ->setSharingMode(oboe::SharingMode::Exclusive)
               ->setFormat(oboe::AudioFormat::Float)
               ->setChannelCount(CHANNELS)
               ->setSampleRate(SAMPLE_RATE)
               ->setDataCallback(this)
               ->setErrorCallback(this);
        builder.openStream(mStream);
        mStream->requestStart();
    }

    void stop() {
        std::lock_guard<std::mutex> lock(mStreamMutex);
        mIsRunning = false;
        mDecodeCV.notify_all();
        if (mDecodeThread.joinable()) mDecodeThread.join();
        if (mStream) { mStream->stop(); mStream->close(); mStream.reset(); }
    }

    bool onError(oboe::AudioStream *audioStream, oboe::Result error) override {
        if (error == oboe::Result::ErrorDisconnected) {
            std::thread([this]() { stop(); start(); }).detach();
        }
        return false;
    }

    // --- CONSUMER: AUDIO CALLBACK ---
    oboe::DataCallbackResult onAudioReady(oboe::AudioStream *audioStream, void *audioData, int32_t numFrames) override {
        float *output = static_cast<float *>(audioData);
        int32_t totalSamplesNeeded = numFrames * CHANNELS;
        memset(output, 0, totalSamplesNeeded * sizeof(float));

        std::lock_guard<std::mutex> lock(mBufferMutex);

        size_t availableFrames = mPcmBuffer.size() / SAMPLES_PER_FRAME
                               + mJitterBuffer.size();

        if (mIsBuffering) {
            if (availableFrames >= (size_t)mAdaptive.targetFrames()) {
                mIsBuffering = false;
                LOGI("[Jitter] Buffering Complete. Frames: %zu (target %d)", availableFrames, mAdaptive.targetFrames());
            } else {
                return oboe::DataCallbackResult::Continue;
            }
        }

        if (!mPcmBuffer.empty()) {
            size_t toCopy = std::min((size_t)totalSamplesNeeded, mPcmBuffer.size());
            memcpy(output, mPcmBuffer.data(), toCopy * sizeof(float));
            mPcmBuffer.erase(mPcmBuffer.begin(), mPcmBuffer.begin() + toCopy);
        } else {
            // Ran dry: this is the underrun that drives the adaptive watermark.
            mUnderflowCount++;
            mIsBuffering = true;
            mAdaptive.onUnderrun();
        }

        // Let the controller observe occupancy on the audio clock rather than
        // relying on the decode thread, which sleeps when it has nothing to do.
        mAdaptive.onTick(availableFrames);

        return oboe::DataCallbackResult::Continue;
    }

    // --- PRODUCER: DECODE THREAD ---
    void decodeLoop() {
        setpriority(PRIO_PROCESS, 0, -16);
        LOGI("[Performance] Decode Worker Thread Started.");

        while (mIsRunning) {
            uint64_t currentSeq = 0;
            std::vector<uint8_t> opusData;
            bool isPlc = false;
            uint64_t gapStart = 0;
            uint64_t gapEnd = 0;
            bool requestResend = false;

            {
                std::unique_lock<std::mutex> lock(mBufferMutex);

                // Wait for packets
                mDecodeCV.wait_for(lock, std::chrono::milliseconds(50), [this] {
                    bool lowPcm = (mPcmBuffer.size() / SAMPLES_PER_FRAME) < (size_t)mAdaptive.pcmTargetFrames();
                    return !mIsRunning || (!mJitterBuffer.empty() && (mIsBuffering || lowPcm));
                });

                if (!mIsRunning) break;
                if (mJitterBuffer.empty()) continue;

                // Demand-based decoding: only decode when buffering is in
                // progress or the decoded PCM inventory is running low.
                bool lowPcm = (mPcmBuffer.size() / SAMPLES_PER_FRAME) < (size_t)mAdaptive.pcmTargetFrames();
                if (!mIsBuffering && !lowPcm) continue;

                // Catch-up: drop the oldest frames until the backlog fits the
                // watermark, so latency does not grow without bound.
                size_t bufferedFrames = mJitterBuffer.size() + (mPcmBuffer.size() / SAMPLES_PER_FRAME);
                while (mAdaptive.shouldDiscard(bufferedFrames) && !mJitterBuffer.empty()) {
                    mJitterBuffer.erase(mJitterBuffer.begin());
                    mExpectedSeq++;
                    mDiscardedFrames++;
                    bufferedFrames--;
                    mAdaptive.onDiscarded();
                }
                if (mJitterBuffer.empty()) continue;

                auto it = mJitterBuffer.begin();
                currentSeq = it->first;
                if (mFirstPacket) { mExpectedSeq = currentSeq; mFirstPacket = false; }

                if (currentSeq > mExpectedSeq + 100) {
                    mExpectedSeq = currentSeq;
                    mPcmBuffer.clear();
                }

                if (currentSeq > mExpectedSeq) {
                    // A packet never arrived. Record the gap so the caller can ask
                    // the sender to resend before we conceal audio that still
                    // exists upstream.
                    isPlc = true;
                    gapStart = mExpectedSeq;
                    gapEnd = currentSeq - 1;
                    mExpectedSeq++;
                    requestResend = true;
                } else if (currentSeq < mExpectedSeq) {
                    mJitterBuffer.erase(it);
                    continue;
                } else {
                    // Normal case: pop the packet to decode it outside the lock
                    opusData = std::move(it->second);
                    mJitterBuffer.erase(it);
                    mExpectedSeq++;
                }
            } // MUTEX UNLOCKED HERE

            // Ask for the missing packets. Bounded so a large gap cannot flood
            // the sender with requests.
            //
            // The callback is copied under the lock rather than invoked under it:
            // it crosses into Kotlin and sends a UDP packet, and holding
            // mBufferMutex across that would stall the audio callback.
            if (requestResend) {
                std::function<void(uint64_t)> resend;
                {
                    std::lock_guard<std::mutex> lock(mBufferMutex);
                    resend = mResendCallback;
                }
                if (resend) {
                    for (uint64_t tick = gapStart; tick <= gapEnd && tick - gapStart < 8; tick++) {
                        resend(tick);
                    }
                }
            }

            float decodeOut[MAX_FRAME_SIZE * CHANNELS];
            int decoded = 0;

            if (isPlc) {
                if (mPlcEnabled) {
                    // Standard Opus PLC
                    decoded = opus_decode_float(mOpusDecoder, nullptr, 0, decodeOut, MAX_FRAME_SIZE, 0);
                    if (decoded > 0) mPlcCount++;
                } else {
                    // PLC Disabled: Zero-fill to maintain timing (Silence)
                    memset(decodeOut, 0, MAX_FRAME_SIZE * CHANNELS * sizeof(float));
                    decoded = MAX_FRAME_SIZE;
                }
            } else if (!opusData.empty()) {
                auto dStart = std::chrono::high_resolution_clock::now();
                decoded = opus_decode_float(mOpusDecoder, opusData.data(), opusData.size(), decodeOut, MAX_FRAME_SIZE, 0);
                auto dEnd = std::chrono::high_resolution_clock::now();
                auto dUs = std::chrono::duration_cast<std::chrono::microseconds>(dEnd - dStart).count();

                // Increase threshold to 12ms to clear the screen from minor spikes
                if (dUs > 12000) {
                    LOGI("[Performance] CRITICAL Decode Time: %lld us (Core: %d)", (long long)dUs, sched_getcpu());
                }
            }

            if (decoded > 0) {
                std::lock_guard<std::mutex> lock(mBufferMutex);
                mPcmBuffer.insert(mPcmBuffer.end(), decodeOut, decodeOut + (decoded * CHANNELS));
            }
        }
        LOGI("[Performance] Decode Worker Thread Stopped.");
    }

    void setBufferSize(int size) {
        std::lock_guard<std::mutex> lock(mBufferMutex);
        mAdaptive.configure(size);
        resetLocked();
    }

    void updateAdvancedSettings(int pcmTarget, int catchUpThreshold) {
        std::lock_guard<std::mutex> lock(mBufferMutex);
        mAdaptive.setPcmTarget(pcmTarget);
        mAdaptive.setCatchUp(catchUpThreshold);
        LOGI("[Jitter] Advanced Settings Updated: PCM Target %d, Catch-up +%d",
             mAdaptive.pcmTargetFrames(), mAdaptive.catchUpFrames());
    }

    void setPlcEnabled(bool enabled) {
        mPlcEnabled = enabled;
        LOGI("[Jitter] PLC %s", enabled ? "ENABLED" : "DISABLED");
    }

    void setResendCallback(std::function<void(uint64_t)> cb) {
        std::lock_guard<std::mutex> lock(mBufferMutex);
        mResendCallback = std::move(cb);
    }

    int getBufferDepth() {
        std::lock_guard<std::mutex> lock(mBufferMutex);
        return (int)((mJitterBuffer.size() * SAMPLES_PER_FRAME + mPcmBuffer.size()) / SAMPLES_PER_FRAME);
    }

    // These counters are read from two independent loops (the periodic log line
    // and the stats report), so they must be plain reads. A destructive read
    // would let whichever loop ran first steal the other's samples.
    int getPLCCount() { return mPlcCount.load(); }
    int getUnderflowCount() { return mUnderflowCount.load(); }
    int getDiscardedFrames() { return mDiscardedFrames.load(); }
    int getMaxBufferedFrames() { return mMaxBufferedFrames.load(); }

    // AdaptiveBuffer is mutated from the audio callback, so its fields must be
    // read under the same mutex the callback holds.
    int getTargetFrames() {
        std::lock_guard<std::mutex> lock(mBufferMutex);
        return mAdaptive.targetFrames();
    }
    int getCeilingFrames() {
        std::lock_guard<std::mutex> lock(mBufferMutex);
        return mAdaptive.ceilingFrames();
    }

    /// Percentage of packets that arrived after their media tick had already
    /// been passed over, scaled by 100 so the value survives an int round trip.
    int getLatePercentX100() {
        uint64_t total = mTotalPackets.load();
        if (total == 0) return 0;
        return (int)((mLatePackets.load() * 10000) / total);
    }

    void pushPacket(uint64_t tick, const uint8_t* data, int len) {
        std::lock_guard<std::mutex> lock(mBufferMutex);

        // Cooldown: Ignore packets arriving within 100ms of a reset
        // to prevent sequence misalignment from stale packets.
        auto now = std::chrono::steady_clock::now();
        if (std::chrono::duration_cast<std::chrono::milliseconds>(now - mLastResetTime).count() < 100) {
            return;
        }

        mTotalPackets++;
        // A tick below the expected sequence is late by definition: the decoder
        // has already moved past it.
        if (!mFirstPacket && tick < mExpectedSeq) {
            mLatePackets++;
        }

        mJitterBuffer[tick] = std::vector<uint8_t>(data, data + len);
        if (mJitterBuffer.size() > 50) mJitterBuffer.erase(mJitterBuffer.begin());

        size_t frames = mJitterBuffer.size() + mPcmBuffer.size() / SAMPLES_PER_FRAME;
        int currentMax = mMaxBufferedFrames.load();
        while (frames > (size_t)currentMax && !mMaxBufferedFrames.compare_exchange_weak(currentMax, (int)frames)) {}

        mDecodeCV.notify_one();
    }

private:
    void resetLocked() {
        mJitterBuffer.clear();
        mPcmBuffer.clear();
        mIsBuffering = true;
        mFirstPacket = true;
        mExpectedSeq = 0;
        mPlcCount = 0;
        mUnderflowCount = 0;
        mDiscardedFrames = 0;
        mMaxBufferedFrames = 0;
        mLatePackets = 0;
        mTotalPackets = 0;
        if (mOpusDecoder) opus_decoder_ctl(mOpusDecoder, OPUS_RESET_STATE);
        mLastResetTime = std::chrono::steady_clock::now();
        mDecodeCV.notify_all();
    }

    std::shared_ptr<oboe::AudioStream> mStream;
    std::mutex mStreamMutex;
    std::map<uint64_t, std::vector<uint8_t>> mJitterBuffer;
    std::vector<float> mPcmBuffer;
    std::mutex mBufferMutex;
    std::condition_variable mDecodeCV;
    std::thread mDecodeThread;
    std::atomic<bool> mIsRunning{false};
    std::atomic<bool> mPlcEnabled{true};
    OpusDecoder *mOpusDecoder = nullptr;
    AdaptiveBuffer mAdaptive;
    bool mIsBuffering = true;
    uint64_t mExpectedSeq = 0;
    bool mFirstPacket = true;
    std::atomic<int> mPlcCount{0};
    std::atomic<int> mUnderflowCount{0};
    std::atomic<int> mDiscardedFrames{0};
    std::atomic<int> mMaxBufferedFrames{0};
    std::atomic<uint64_t> mLatePackets{0};
    std::atomic<uint64_t> mTotalPackets{0};
    std::chrono::steady_clock::time_point mLastResetTime;
    std::function<void(uint64_t)> mResendCallback;
};

static AudioEngine gAudioEngine;

// ---------------------------------------------------------------------------
// JNI callback plumbing for resend requests.
//
// The decode thread is a plain std::thread, not an Android-managed one, so it
// has no JNIEnv. It has to attach itself to the JVM before it can call back into
// Kotlin to send the request packet.
// ---------------------------------------------------------------------------
static JavaVM* gJvm = nullptr;
static jobject gResendCallbackObj = nullptr;
static jmethodID gResendMethod = nullptr;

extern "C" {

// Must be inside `extern "C"`: the JVM looks this symbol up by its unmangled
// name. Declared outside, it would be C++-mangled and never found, leaving
// gJvm null and silently disabling every callback that needs to attach a
// native thread to the VM.
JNIEXPORT jint JNICALL JNI_OnLoad(JavaVM* vm, void* reserved) {
    (void)reserved;
    gJvm = vm;
    return JNI_VERSION_1_6;
}

JNIEXPORT jint JNICALL Java_com_example_audiobtbridge_NativeBridge_initNative(JNIEnv *env, jobject thiz) { (void)env; (void)thiz; gAudioEngine.start(); return 0; }
JNIEXPORT void JNICALL Java_com_example_audiobtbridge_NativeBridge_stopNative(JNIEnv *env, jobject thiz) { (void)env; (void)thiz; gAudioEngine.stop(); }
JNIEXPORT void JNICALL Java_com_example_audiobtbridge_NativeBridge_resetAudio(JNIEnv *env, jobject thiz) { (void)env; (void)thiz; gAudioEngine.resetInternal(); }
JNIEXPORT void JNICALL Java_com_example_audiobtbridge_NativeBridge_setBufferSize(JNIEnv *env, jobject thiz, jint size) { (void)env; (void)thiz; gAudioEngine.setBufferSize(size); }
JNIEXPORT void JNICALL Java_com_example_audiobtbridge_NativeBridge_setPlcEnabled(JNIEnv *env, jobject thiz, jboolean enabled) { (void)env; (void)thiz; gAudioEngine.setPlcEnabled(enabled); }
JNIEXPORT void JNICALL Java_com_example_audiobtbridge_NativeBridge_updateAdvancedSettings(JNIEnv *env, jobject thiz, jint pcmTarget, jint catchUpThreshold) {
    (void)env; (void)thiz;
    gAudioEngine.updateAdvancedSettings(pcmTarget, catchUpThreshold);
}
JNIEXPORT jint JNICALL Java_com_example_audiobtbridge_NativeBridge_getBufferDepth(JNIEnv *env, jobject thiz) { (void)env; (void)thiz; return gAudioEngine.getBufferDepth(); }
JNIEXPORT jint JNICALL Java_com_example_audiobtbridge_NativeBridge_getPLCCount(JNIEnv *env, jobject thiz) { (void)env; (void)thiz; return gAudioEngine.getPLCCount(); }
JNIEXPORT jint JNICALL Java_com_example_audiobtbridge_NativeBridge_getUnderflowCount(JNIEnv *env, jobject thiz) { (void)env; (void)thiz; return gAudioEngine.getUnderflowCount(); }
JNIEXPORT jint JNICALL Java_com_example_audiobtbridge_NativeBridge_getDiscardedFrames(JNIEnv *env, jobject thiz) { (void)env; (void)thiz; return gAudioEngine.getDiscardedFrames(); }
JNIEXPORT jint JNICALL Java_com_example_audiobtbridge_NativeBridge_getMaxBufferedFrames(JNIEnv *env, jobject thiz) { (void)env; (void)thiz; return gAudioEngine.getMaxBufferedFrames(); }
JNIEXPORT jint JNICALL Java_com_example_audiobtbridge_NativeBridge_getTargetFrames(JNIEnv *env, jobject thiz) { (void)env; (void)thiz; return gAudioEngine.getTargetFrames(); }
JNIEXPORT jint JNICALL Java_com_example_audiobtbridge_NativeBridge_getCeilingFrames(JNIEnv *env, jobject thiz) { (void)env; (void)thiz; return gAudioEngine.getCeilingFrames(); }
JNIEXPORT jint JNICALL Java_com_example_audiobtbridge_NativeBridge_getLatePercentX100(JNIEnv *env, jobject thiz) { (void)env; (void)thiz; return gAudioEngine.getLatePercentX100(); }

JNIEXPORT void JNICALL Java_com_example_audiobtbridge_NativeBridge_setResendCallback(JNIEnv *env, jobject thiz, jobject callback) {
    (void)thiz;
    if (gResendCallbackObj != nullptr) {
        env->DeleteGlobalRef(gResendCallbackObj);
        gResendCallbackObj = nullptr;
    }
    gResendMethod = nullptr;

    if (callback == nullptr) {
        gAudioEngine.setResendCallback(nullptr);
        return;
    }

    gResendCallbackObj = env->NewGlobalRef(callback);
    jclass cls = env->GetObjectClass(callback);
    gResendMethod = env->GetMethodID(cls, "onResendRequest", "(J)V");
    env->DeleteLocalRef(cls);

    if (gResendMethod == nullptr) {
        LOGI("[Jitter] Resend callback has no onResendRequest(long) method; retransmission disabled");
        env->DeleteGlobalRef(gResendCallbackObj);
        gResendCallbackObj = nullptr;
        gAudioEngine.setResendCallback(nullptr);
        return;
    }
    LOGI("[Jitter] Resend callback registered; retransmission enabled");

    gAudioEngine.setResendCallback([](uint64_t tick) {
        if (gJvm == nullptr || gResendCallbackObj == nullptr || gResendMethod == nullptr) return;

        JNIEnv* env = nullptr;
        bool attached = false;
        jint status = gJvm->GetEnv(reinterpret_cast<void**>(&env), JNI_VERSION_1_6);
        if (status == JNI_EDETACHED) {
            if (gJvm->AttachCurrentThread(&env, nullptr) != JNI_OK) return;
            attached = true;
        } else if (status != JNI_OK) {
            return;
        }

        env->CallVoidMethod(gResendCallbackObj, gResendMethod, static_cast<jlong>(tick));

        if (env->ExceptionCheck()) {
            env->ExceptionDescribe();
            env->ExceptionClear();
        }
        if (attached) gJvm->DetachCurrentThread();
    });
}

JNIEXPORT void JNICALL Java_com_example_audiobtbridge_NativeBridge_writeToNativeBuffer(JNIEnv *env, jobject thiz, jbyteArray data, jint length) {
    (void)thiz;
    jbyte* buffer = env->GetByteArrayElements(data, nullptr);
    if (buffer != nullptr && length > PACKET_HEADER_LEN) {
        // The Kotlin side strips the "AS2P_AUDIO" prefix, so what arrives here
        // starts at the media tick.
        uint64_t tick = 0;
        memcpy(&tick, buffer, 8);
        gAudioEngine.pushPacket(tick, reinterpret_cast<const uint8_t*>(buffer + PACKET_HEADER_LEN), (int)length - PACKET_HEADER_LEN);
    }
    if (buffer != nullptr) env->ReleaseByteArrayElements(data, buffer, JNI_ABORT);
}
}
