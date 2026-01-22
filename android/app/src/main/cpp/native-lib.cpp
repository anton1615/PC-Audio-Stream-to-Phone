#include <jni.h>
#include <string>
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

#define LOG_TAG "AS2P_Native"
#define LOGI(...) __android_log_print(ANDROID_LOG_INFO, LOG_TAG, __VA_ARGS__)

const int SAMPLE_RATE = 48000;
const int CHANNELS = 2;
const int MAX_FRAME_SIZE = 960; 

class AudioEngine : public oboe::AudioStreamDataCallback, public oboe::AudioStreamErrorCallback {
public:
    AudioEngine() { createDecoder(); }
    ~AudioEngine() { destroyDecoder(); }

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
        mDecodedPcmBuffer.clear();
        mIsBuffering = true;
        mFirstPacket = true;
        mExpectedSeq = 0;
        mPlcCount = 0;
        if (mOpusDecoder) opus_decoder_ctl(mOpusDecoder, OPUS_RESET_STATE);
        LOGI(">>> BUFFER CLEARED <<<");
    }

    void start() {
        std::lock_guard<std::mutex> lock(mStreamMutex);
        if (mStream) return;
        resetInternal();
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
        if (mStream) { mStream->stop(); mStream->close(); mStream.reset(); }
    }

    bool onError(oboe::AudioStream *audioStream, oboe::Result error) override {
        if (error == oboe::Result::ErrorDisconnected) {
            std::thread([this]() {
                stop();
                start();
            }).detach();
        }
        return false;
    }

    oboe::DataCallbackResult onAudioReady(oboe::AudioStream *audioStream, void *audioData, int32_t numFrames) override {
        // --- THREAD AFFINITY (One-time setup for this thread) ---
        static thread_local bool affinitySet = false;
        if (!affinitySet) {
            cpu_set_t cpuset;
            CPU_ZERO(&cpuset);
            // On Tensor GS101 (Pixel 6a): 4,5 are Medium, 6,7 are Big. 
            // We target these 4 high-performance cores.
            CPU_SET(4, &cpuset);
            CPU_SET(5, &cpuset);
            CPU_SET(6, &cpuset);
            CPU_SET(7, &cpuset);
            if (sched_setaffinity(0, sizeof(cpu_set_t), &cpuset) == 0) {
                LOGI("[Performance] Thread Affinity set to cores 4-7");
            } else {
                LOGI("[Performance] Failed to set Thread Affinity");
            }
            affinitySet = true;
        }

        auto startTime = std::chrono::high_resolution_clock::now();
        float *output = static_cast<float *>(audioData);
        int32_t totalSamplesNeeded = numFrames * CHANNELS;
        memset(output, 0, totalSamplesNeeded * sizeof(float));

        std::lock_guard<std::mutex> lock(mBufferMutex);
        
        // --- CATCH-UP LOGIC ---
        while (mJitterBuffer.size() > (size_t)(mTargetBufferSize + 2)) { // Allow a bit more slack
            LOGI("[Jitter] Drop Packet (Buffer Overflow): Seq %llu, Size: %zu", mJitterBuffer.begin()->first, mJitterBuffer.size());
            mJitterBuffer.erase(mJitterBuffer.begin());
            mExpectedSeq++;
        }

        if (mIsBuffering) {
            if (mJitterBuffer.size() >= (size_t)mTargetBufferSize) {
                mIsBuffering = false;
                LOGI("[Jitter] Buffering Complete. Starting playback.");
            } else return oboe::DataCallbackResult::Continue;
        }

        int packetsDecodedThisTurn = 0;
        while (mDecodedPcmBuffer.size() < (size_t)totalSamplesNeeded) {
            if (mJitterBuffer.empty()) break;

            auto it = mJitterBuffer.begin();
            uint64_t currentSeq = it->first;
            
            if (mFirstPacket) { mExpectedSeq = currentSeq; mFirstPacket = false; }

            // 嚴重跳號檢查
            if (currentSeq > mExpectedSeq + 200) { 
                LOGI("[Jitter] Hard Reset: Large Jump (%llu -> %llu)", mExpectedSeq, currentSeq);
                mExpectedSeq = currentSeq; 
                mDecodedPcmBuffer.clear();
            }

            if (currentSeq > mExpectedSeq) {
                // PLC
                float decodeOut[MAX_FRAME_SIZE * CHANNELS];
                int decoded = opus_decode_float(mOpusDecoder, nullptr, 0, decodeOut, MAX_FRAME_SIZE, 0);
                if (decoded > 0) {
                    mDecodedPcmBuffer.insert(mDecodedPcmBuffer.end(), decodeOut, decodeOut + (decoded * CHANNELS));
                    mExpectedSeq++;
                    mPlcCount++;
                }
                continue;
            } else if (currentSeq < mExpectedSeq) {
                // 收到過期封包
                mJitterBuffer.erase(it);
                continue;
            }

            float decodeOut[MAX_FRAME_SIZE * CHANNELS];
            auto dStart = std::chrono::high_resolution_clock::now();
            int decoded = opus_decode_float(mOpusDecoder, it->second.data(), it->second.size(), decodeOut, MAX_FRAME_SIZE, 0);
            auto dEnd = std::chrono::high_resolution_clock::now();
            auto dDuration = std::chrono::duration_cast<std::chrono::microseconds>(dEnd - dStart).count();
            
            // 如果解碼時間異常（例如 > 5ms），則記錄
            if (dDuration > 5000) {
                LOGI("[Performance] High Decode Time: %lld us", dDuration);
            }

            if (decoded > 0) {
                mDecodedPcmBuffer.insert(mDecodedPcmBuffer.end(), decodeOut, decodeOut + (decoded * CHANNELS));
                packetsDecodedThisTurn++;
            }
            mExpectedSeq++;
            mJitterBuffer.erase(it);
        }

        if (!mDecodedPcmBuffer.empty()) {
            size_t toCopy = std::min((size_t)totalSamplesNeeded, mDecodedPcmBuffer.size());
            memcpy(output, mDecodedPcmBuffer.data(), toCopy * sizeof(float));
            mDecodedPcmBuffer.erase(mDecodedPcmBuffer.begin(), mDecodedPcmBuffer.begin() + toCopy);
        } else {
            // Underrun
            LOGI("[Jitter] Underrun: Buffer empty during callback.");
            mIsBuffering = true;
        }

        auto endTime = std::chrono::high_resolution_clock::now();
        auto totalDuration = std::chrono::duration_cast<std::chrono::microseconds>(endTime - startTime).count();
        if (totalDuration > 15000) { // Callback > 15ms (Total turn is 20ms)
            LOGI("[Performance] CRITICAL: Callback took %lld us", totalDuration);
        }
        
        return oboe::DataCallbackResult::Continue;
    }

    void setBufferSize(int size) {
        mTargetBufferSize = size;
        {
            std::lock_guard<std::mutex> lock(mStreamMutex);
            if (mStream) {
                // Set system buffer to 2 * burst size (typical for low latency)
                // or match our jitter buffer size in frames
                int32_t framesPerPacket = 960; 
                mStream->setBufferSizeInFrames(size * framesPerPacket);
            }
        }
        resetInternal();
    }

    int getBufferDepth() { std::lock_guard<std::mutex> lock(mBufferMutex); return (int)mJitterBuffer.size(); }
    int getPLCCount() { return mPlcCount.exchange(0); }

    void pushPacket(uint64_t seq, const uint8_t* data, int len) {
        std::lock_guard<std::mutex> lock(mBufferMutex);
        mJitterBuffer[seq] = std::vector<uint8_t>(data, data + len);
        if (mJitterBuffer.size() > 100) mJitterBuffer.erase(mJitterBuffer.begin());
    }

private:
    std::shared_ptr<oboe::AudioStream> mStream;
    std::mutex mStreamMutex;
    std::map<uint64_t, std::vector<uint8_t>> mJitterBuffer;
    std::vector<float> mDecodedPcmBuffer;
    std::mutex mBufferMutex;
    OpusDecoder *mOpusDecoder = nullptr;
    bool mIsBuffering = true;
    int mTargetBufferSize = 5;
    uint64_t mExpectedSeq = 0;
    bool mFirstPacket = true;
    std::atomic<int> mPlcCount{0};
};

static AudioEngine gAudioEngine;

extern "C" {
JNIEXPORT jint JNICALL Java_com_example_audiobtbridge_NativeBridge_initNative(JNIEnv *env, jobject thiz) { gAudioEngine.start(); return 0; }
JNIEXPORT void JNICALL Java_com_example_audiobtbridge_NativeBridge_stopNative(JNIEnv *env, jobject thiz) { gAudioEngine.stop(); }
JNIEXPORT void JNICALL Java_com_example_audiobtbridge_NativeBridge_resetAudio(JNIEnv *env, jobject thiz) { gAudioEngine.resetInternal(); }
JNIEXPORT void JNICALL Java_com_example_audiobtbridge_NativeBridge_setBufferSize(JNIEnv *env, jobject thiz, jint size) { gAudioEngine.setBufferSize(size); }
JNIEXPORT jint JNICALL Java_com_example_audiobtbridge_NativeBridge_getBufferDepth(JNIEnv *env, jobject thiz) { return gAudioEngine.getBufferDepth(); }
JNIEXPORT jint JNICALL Java_com_example_audiobtbridge_NativeBridge_getPLCCount(JNIEnv *env, jobject thiz) { return gAudioEngine.getPLCCount(); }
JNIEXPORT void JNICALL Java_com_example_audiobtbridge_NativeBridge_writeToNativeBuffer(JNIEnv *env, jobject thiz, jbyteArray data, jint length) {
    jbyte* buffer = env->GetByteArrayElements(data, nullptr);
    if (length >= 16) { 
        uint64_t seq = 0;
        memcpy(&seq, buffer, 8);
        if (length > 16) {
            gAudioEngine.pushPacket(seq, reinterpret_cast<const uint8_t*>(buffer + 16), (int)length - 16);
            // Log periodically or on large gaps? 
            // Let's log every 100 packets to verify throughput without spamming
            static int pCount = 0;
            if (++pCount % 100 == 0) {
                // LOGI("[Network] Received 100 packets. Last Seq: %llu", seq);
            }
        }
    } else {
        LOGI("[Network] CRITICAL: Received tiny packet, length: %d", length);
    }
    env->ReleaseByteArrayElements(data, buffer, JNI_ABORT);
}
}