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
               ->setSharingMode(oboe::SharingMode::Shared)      
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
        float *output = static_cast<float *>(audioData);
        int32_t totalSamplesNeeded = numFrames * CHANNELS;
        memset(output, 0, totalSamplesNeeded * sizeof(float));

        std::lock_guard<std::mutex> lock(mBufferMutex);
        
        // --- CATCH-UP LOGIC ---
        // If buffer is building up beyond target + 1, drop oldest to maintain low latency
        while (mJitterBuffer.size() > (size_t)(mTargetBufferSize + 1)) {
            mJitterBuffer.erase(mJitterBuffer.begin());
            mExpectedSeq++; // Keep expected seq in sync
        }

        if (mIsBuffering) {
            if (mJitterBuffer.size() >= (size_t)mTargetBufferSize) mIsBuffering = false;
            else return oboe::DataCallbackResult::Continue;
        }

        while (mDecodedPcmBuffer.size() < (size_t)totalSamplesNeeded) {
            if (mJitterBuffer.empty()) break;

            auto it = mJitterBuffer.begin();
            uint64_t currentSeq = it->first;
            
            if (mFirstPacket) { mExpectedSeq = currentSeq; mFirstPacket = false; }

            // 嚴格對齊，不符合預期的序號直接跳過（防止機械音）
            if (currentSeq > mExpectedSeq + 100) { 
                mExpectedSeq = currentSeq; 
                mDecodedPcmBuffer.clear();
            }

            if (currentSeq > mExpectedSeq) {
                // PLC: 丟包補償（解碼空數據）
                float decodeOut[MAX_FRAME_SIZE * CHANNELS];
                int decoded = opus_decode_float(mOpusDecoder, nullptr, 0, decodeOut, MAX_FRAME_SIZE, 0);
                if (decoded > 0) {
                    mDecodedPcmBuffer.insert(mDecodedPcmBuffer.end(), decodeOut, decodeOut + (decoded * CHANNELS));
                    mExpectedSeq++;
                    mPlcCount++;
                }
                continue;
            } else if (currentSeq < mExpectedSeq) {
                mJitterBuffer.erase(it);
                continue;
            }

            float decodeOut[MAX_FRAME_SIZE * CHANNELS];
            int decoded = opus_decode_float(mOpusDecoder, it->second.data(), it->second.size(), decodeOut, MAX_FRAME_SIZE, 0);
            if (decoded > 0) {
                mDecodedPcmBuffer.insert(mDecodedPcmBuffer.end(), decodeOut, decodeOut + (decoded * CHANNELS));
            } else if (decoded < 0) {
                static int lastErr = 0;
                if (decoded != lastErr) {
                    __android_log_print(ANDROID_LOG_ERROR, LOG_TAG, "Opus Decode Error: %d", decoded);
                    lastErr = decoded;
                }
            }
            mExpectedSeq++;
            mJitterBuffer.erase(it);
        }

        if (!mDecodedPcmBuffer.empty()) {
            size_t toCopy = std::min((size_t)totalSamplesNeeded, mDecodedPcmBuffer.size());
            memcpy(output, mDecodedPcmBuffer.data(), toCopy * sizeof(float));
            mDecodedPcmBuffer.erase(mDecodedPcmBuffer.begin(), mDecodedPcmBuffer.begin() + toCopy);
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
    // V8 Protocol: AS2P_AUDIO(10) + SEQ(8) + TS(8) + PAYLOAD
    // Kotlin layer strips the prefix (26 bytes) and just sends SEQ(8) + TS(8) + PAYLOAD... wait.
    // In AudioService.kt we decided: 
    // "if (len > 26) { ... System.arraycopy(data, 26, payload, 0, payloadSize) ... NativeBridge.writeToNativeBuffer(payload) }"
    // BUT wait, if we stripped 26 bytes, then the payload passed here is JUST the Opus data!
    // We lost the Sequence Number if we strip 26 bytes. 
    // The prefix is 10 bytes ("AS2P_AUDIO").
    // Then 8 bytes Seq, 8 bytes TS.
    // If we strip 26 bytes in Kotlin, we strip Seq and TS too.
    
    // Correct Logic for AudioService.kt was:
    // Strip only "AS2P_AUDIO" (10 bytes).
    // Let's re-read AudioService.kt logic I just wrote:
    // "if (len > 26) { val payloadSize = len - 26 ... System.arraycopy(data, 26, payload..."
    // ERROR: I implemented it to strip Seq and TS in Kotlin. That's bad.
    
    // CORRECTION STRATEGY:
    // I will modify this C++ function to assume the input is [Seq(8) + TS(8) + OpusData].
    // I need to go back and fix AudioService.kt to only strip the first 10 bytes (AS2P_AUDIO).
    // Or, I can adapt this C++ to receive the FULL packet and do the parsing here, which is safer.
    
    // Let's assume for this step that AudioService PASSES everything starting from Seq.
    // So AudioService should strip 10 bytes.
    // I will fix AudioService in the NEXT turn.
    // Here, I will implement expecting [Seq(8) + TS(8) + Opus].
    
    jbyte* buffer = env->GetByteArrayElements(data, nullptr);
    if (length >= 16) { // Seq(8) + TS(8)
        uint64_t seq = 0;
        memcpy(&seq, buffer, 8);
        // Skip TS (next 8 bytes), so data starts at offset 16
        if (length > 16) {
            gAudioEngine.pushPacket(seq, reinterpret_cast<const uint8_t*>(buffer + 16), (int)length - 16);
        }
    }
    env->ReleaseByteArrayElements(data, buffer, JNI_ABORT);
}
}