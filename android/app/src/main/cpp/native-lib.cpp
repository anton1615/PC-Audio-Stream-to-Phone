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
#include <sys/resource.h>
#include <condition_variable>

#define LOG_TAG "AS2P_Native"
#define LOGI(...) __android_log_print(ANDROID_LOG_INFO, LOG_TAG, __VA_ARGS__)

const int SAMPLE_RATE = 48000;
const int CHANNELS = 2;
const int MAX_FRAME_SIZE = 960; 

class AudioEngine : public oboe::AudioStreamDataCallback, public oboe::AudioStreamErrorCallback {
public:
    AudioEngine() { createDecoder(); }
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
        if (mOpusDecoder) opus_decoder_ctl(mOpusDecoder, OPUS_RESET_STATE);
        LOGI(">>> ENGINE RESET <<<");
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
        
        // Use user-defined buffering target (in packets, 1 packet = 20ms = 960 samples * 2 channels)
        size_t samplesTarget = (size_t)mTargetBufferSize * 1920;

        if (mIsBuffering) {
            if (mPcmBuffer.size() >= samplesTarget) {
                mIsBuffering = false;
                LOGI("[Jitter] Buffering Complete. PCM: %zu", mPcmBuffer.size());
            } else return oboe::DataCallbackResult::Continue;
        }

        if (!mPcmBuffer.empty()) {
            size_t toCopy = std::min((size_t)totalSamplesNeeded, mPcmBuffer.size());
            memcpy(output, mPcmBuffer.data(), toCopy * sizeof(float));
            mPcmBuffer.erase(mPcmBuffer.begin(), mPcmBuffer.begin() + toCopy);
        } else {
            mIsBuffering = true;
        }
        
        return oboe::DataCallbackResult::Continue;
    }

    // --- PRODUCER: DECODE THREAD ---
    void decodeLoop() {
        setpriority(PRIO_PROCESS, 0, -16);
        LOGI("[Performance] Decode Worker Thread Started.");
        
        while (mIsRunning) {
            std::unique_lock<std::mutex> lock(mBufferMutex);
            mDecodeCV.wait_for(lock, std::chrono::milliseconds(50), [this] {
                return !mIsRunning || !mJitterBuffer.empty();
            });

            if (!mIsRunning) break;
            if (mJitterBuffer.empty()) continue;

            // Catch-up: Keep total latency (Jitter + PCM) at Target + 1
            while (mJitterBuffer.size() + (mPcmBuffer.size() / 1920) > (size_t)(mTargetBufferSize + 1)) {
                if (!mJitterBuffer.empty()) {
                    mJitterBuffer.erase(mJitterBuffer.begin());
                    mExpectedSeq++;
                } else break;
            }

            if (mJitterBuffer.empty()) continue;

            auto it = mJitterBuffer.begin();
            uint64_t currentSeq = it->first;
            if (mFirstPacket) { mExpectedSeq = currentSeq; mFirstPacket = false; }

            if (currentSeq > mExpectedSeq + 100) {
                mExpectedSeq = currentSeq;
                mPcmBuffer.clear();
            }

            float decodeOut[MAX_FRAME_SIZE * CHANNELS];
            int decoded = 0;

            if (currentSeq > mExpectedSeq) {
                decoded = opus_decode_float(mOpusDecoder, nullptr, 0, decodeOut, MAX_FRAME_SIZE, 0);
                if (decoded > 0) mPlcCount++;
                mExpectedSeq++;
            } else if (currentSeq < mExpectedSeq) {
                mJitterBuffer.erase(it);
                continue;
            } else {
                auto dStart = std::chrono::high_resolution_clock::now();
                decoded = opus_decode_float(mOpusDecoder, it->second.data(), it->second.size(), decodeOut, MAX_FRAME_SIZE, 0);
                auto dEnd = std::chrono::high_resolution_clock::now();
                auto dUs = std::chrono::duration_cast<std::chrono::microseconds>(dEnd - dStart).count();
                
                if (dUs > 5000) {
                    LOGI("[Performance] Worker High Decode Time: %lld us (Core: %d)", dUs, sched_getcpu());
                }

                mExpectedSeq++;
                mJitterBuffer.erase(it);
            }

            if (decoded > 0) {
                mPcmBuffer.insert(mPcmBuffer.end(), decodeOut, decodeOut + (decoded * CHANNELS));
            }
        }
        LOGI("[Performance] Decode Worker Thread Stopped.");
    }

    void setBufferSize(int size) {
        // Validation: Clamp between 1 (20ms) and 25 (500ms) to prevent overflow/DoS
        mTargetBufferSize = (size < 1) ? 1 : (size > 25 ? 25 : size);
        resetInternal();
    }

    int getBufferDepth() { 
        std::lock_guard<std::mutex> lock(mBufferMutex); 
        return (int)((mJitterBuffer.size() * 1920 + mPcmBuffer.size()) / 1920); 
    }
    int getPLCCount() { return mPlcCount.exchange(0); }

    void pushPacket(uint64_t seq, const uint8_t* data, int len) {
        std::lock_guard<std::mutex> lock(mBufferMutex);
        mJitterBuffer[seq] = std::vector<uint8_t>(data, data + len);
        if (mJitterBuffer.size() > 50) mJitterBuffer.erase(mJitterBuffer.begin());
        mDecodeCV.notify_one();
    }

private:
    std::shared_ptr<oboe::AudioStream> mStream;
    std::mutex mStreamMutex;
    std::map<uint64_t, std::vector<uint8_t>> mJitterBuffer;
    std::vector<float> mPcmBuffer;
    std::mutex mBufferMutex;
    std::condition_variable mDecodeCV;
    std::thread mDecodeThread;
    std::atomic<bool> mIsRunning{false};
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
        }
    }
    env->ReleaseByteArrayElements(data, buffer, JNI_ABORT);
}
}