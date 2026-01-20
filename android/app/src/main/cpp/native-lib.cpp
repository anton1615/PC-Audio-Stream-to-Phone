#include <jni.h>
#include <string>
#include <android/log.h>
#include <oboe/Oboe.h>
#include <vector>
#include <map>
#include <mutex>
#include <opus.h>

#define LOG_TAG "AudioBT_Native"
#define LOGI(...) __android_log_print(ANDROID_LOG_INFO, LOG_TAG, __VA_ARGS__)
#define LOGE(...) __android_log_print(ANDROID_LOG_ERROR, LOG_TAG, __VA_ARGS__)

const int SAMPLE_RATE = 48000;
const int CHANNELS = 2;
const int MAX_FRAME_SIZE = 960; 

class AudioEngine : public oboe::AudioStreamDataCallback, public oboe::AudioStreamErrorCallback {
public:
    AudioEngine() {
        int error;
        mOpusDecoder = opus_decoder_create(SAMPLE_RATE, CHANNELS, &error);
        if (error != OPUS_OK) {
            LOGE("Failed to create Opus decoder: %d", error);
        }
    }

    ~AudioEngine() {
        if (mOpusDecoder) {
            opus_decoder_destroy(mOpusDecoder);
            mOpusDecoder = nullptr;
        }
    }

    void start() {
        std::lock_guard<std::mutex> lock(mStreamMutex);
        if (mStream) return;

        // 清空舊緩衝區，防止重啟後聽到舊聲音
        {
            std::lock_guard<std::mutex> bufferLock(mBufferMutex);
            mJitterBuffer.clear();
            mDecodedPcmBuffer.clear();
            mIsBuffering = true;
            mFirstPacket = true;
            mExpectedSeq = 0;
            mPLCCount = 0;
        }

        oboe::AudioStreamBuilder builder;
        builder.setDirection(oboe::Direction::Output)
               ->setPerformanceMode(oboe::PerformanceMode::LowLatency) 
               ->setSharingMode(oboe::SharingMode::Shared)      
               ->setFormat(oboe::AudioFormat::Float)
               ->setChannelCount(CHANNELS)
               ->setSampleRate(SAMPLE_RATE)
               ->setUsage(oboe::Usage::Media)
               ->setContentType(oboe::ContentType::Music)
               ->setDataCallback(this)
               ->setErrorCallback(this); // 監聽設備切換錯誤

        oboe::Result result = builder.openStream(mStream);
        if (result != oboe::Result::OK) {
            LOGE("Failed to open stream: %s", oboe::convertToText(result));
            return;
        }

        // 設置 Oboe 內部緩衝區大小為 3 個 Burst，增加對高位元率背景抖動的容忍度
        mStream->setBufferSizeInFrames(mStream->getFramesPerBurst() * 3);

        result = mStream->requestStart();
        if (result != oboe::Result::OK) {
            LOGE("Failed to start stream: %s", oboe::convertToText(result));
            mStream->close();
            mStream.reset();
        }
    }

    // 當藍牙連線或中斷導致設備失效時，自動重啟
    void onErrorAfterClose(oboe::AudioStream *stream, oboe::Result error) override {
        if (error == oboe::Result::ErrorDisconnected) {
            LOGI("Audio device disconnected, restarting stream...");
            start();
        }
    }

    void stop() {
        std::lock_guard<std::mutex> lock(mStreamMutex);
        if (mStream) {
            mStream->stop();
            mStream->close();
            mStream.reset();
        }
    }

    oboe::DataCallbackResult onAudioReady(oboe::AudioStream *audioStream, void *audioData, int32_t numFrames) override {
        float *output = static_cast<float *>(audioData);
        int32_t totalSamplesNeeded = numFrames * CHANNELS;
        
        // 優先初始化輸出為 0
        for (int i = 0; i < totalSamplesNeeded; ++i) output[i] = 0.0f;

        std::unique_lock<std::mutex> lock(mBufferMutex, std::try_to_lock);
        if (!lock.owns_lock()) return oboe::DataCallbackResult::Continue; // 如果拿不到鎖，直接跳過這幀，避免爆音
        
        int threshold = std::max(1, mTargetBufferSize);
        if (mIsBuffering) {
            if (mJitterBuffer.size() >= (size_t)threshold) {
                mIsBuffering = false;
                LOGI("Buffering complete, starting playback");
            } else {
                return oboe::DataCallbackResult::Continue;
            }
        }

        while (mDecodedPcmBuffer.size() < totalSamplesNeeded && (!mJitterBuffer.empty() || !mFirstPacket)) {
            if (mJitterBuffer.empty()) {
                // Buffer is empty but we've started playback, perform PLC to smooth out silence
                float decodeOut[MAX_FRAME_SIZE * CHANNELS];
                int decodedFrames = opus_decode_float(mOpusDecoder, nullptr, 0, decodeOut, MAX_FRAME_SIZE, 0);
                if (decodedFrames > 0) {
                    mDecodedPcmBuffer.insert(mDecodedPcmBuffer.end(), decodeOut, decodeOut + (decodedFrames * CHANNELS));
                    mExpectedSeq++;
                    mPLCCount++;
                    if (mPLCCount % 50 == 0) LOGI("PLC triggered due to empty buffer (continuous count: %d)", mPLCCount);
                } else {
                    break; 
                }
                continue;
            }

            auto it = mJitterBuffer.begin();
            uint64_t currentSeq = it->first;

            if (mFirstPacket) {
                mExpectedSeq = currentSeq;
                mFirstPacket = false;
            }

            if (currentSeq > mExpectedSeq) {
                // Gap detected, trigger PLC for missing packet
                float decodeOut[MAX_FRAME_SIZE * CHANNELS];
                int decodedFrames = opus_decode_float(mOpusDecoder, nullptr, 0, decodeOut, MAX_FRAME_SIZE, 0);
                if (decodedFrames > 0) {
                    mDecodedPcmBuffer.insert(mDecodedPcmBuffer.end(), decodeOut, decodeOut + (decodedFrames * CHANNELS));
                    mExpectedSeq++;
                    mPLCCount++;
                    LOGI("PLC triggered for sequence gap: expected %llu, got %llu", (unsigned long long)mExpectedSeq - 1, (unsigned long long)currentSeq);
                    continue; // Re-evaluate with same it
                }
            } else if (currentSeq < mExpectedSeq) {
                // Late packet, discard it to maintain sync
                mJitterBuffer.erase(it);
                continue;
            }

            // Normal sequential decode
            mLastSeq = currentSeq;
            float decodeOut[MAX_FRAME_SIZE * CHANNELS];
            int decodedFrames = opus_decode_float(mOpusDecoder, it->second.data(), it->second.size(), decodeOut, MAX_FRAME_SIZE, 0);

            if (decodedFrames > 0) {
                mDecodedPcmBuffer.insert(mDecodedPcmBuffer.end(), decodeOut, decodeOut + (decodedFrames * CHANNELS));
            }
            mExpectedSeq++;
            mJitterBuffer.erase(it);
        }

        if (!mDecodedPcmBuffer.empty()) {
            size_t samplesToCopy = std::min((size_t)totalSamplesNeeded, mDecodedPcmBuffer.size());
            memcpy(output, mDecodedPcmBuffer.data(), samplesToCopy * sizeof(float));
            mDecodedPcmBuffer.erase(mDecodedPcmBuffer.begin(), mDecodedPcmBuffer.begin() + samplesToCopy);
        } else {
            mIsBuffering = true;
        }
        
        return oboe::DataCallbackResult::Continue;
    }

    void setBufferSize(int size) {
        std::lock_guard<std::mutex> lock(mBufferMutex);
        mTargetBufferSize = size;
        
        // 不論變大變小，只要變動就強制重啟緩衝
        // 清空舊緩衝區，防止不同 Bitrate 混合導致的雜音或解碼異常
                mJitterBuffer.clear();
                mDecodedPcmBuffer.clear();
                mIsBuffering = true;
                mFirstPacket = true;
                mExpectedSeq = 0;
                mPLCCount = 0;
        
                LOGI("Buffer size updated to %d, re-buffering forced...", mTargetBufferSize);    }

    int getBufferDepth() {
        std::lock_guard<std::mutex> lock(mBufferMutex);
        return (int)mJitterBuffer.size();
    }

    int getPLCCount() {
        std::lock_guard<std::mutex> lock(mBufferMutex);
        return mPLCCount;
    }

    uint64_t getLastSequence() {
        return mLastSeq;
    }

    void pushPacket(uint64_t seq, const uint8_t* data, int len) {
        if (len > 2048) {
            LOGE("Received excessively large packet: %d bytes, ignoring.", len);
            return;
        }
        std::lock_guard<std::mutex> lock(mBufferMutex);
        mJitterBuffer[seq] = std::vector<uint8_t>(data, data + len);
        mLastSeq = seq;
        
        if (mJitterBuffer.size() > (size_t)mTargetBufferSize) {
            mJitterBuffer.erase(mJitterBuffer.begin());
        }
    }

private:
    std::shared_ptr<oboe::AudioStream> mStream;
    std::mutex mStreamMutex;
    std::map<uint64_t, std::vector<uint8_t>> mJitterBuffer;
    std::vector<float> mDecodedPcmBuffer;
    std::mutex mBufferMutex;
    OpusDecoder *mOpusDecoder = nullptr;
        bool mIsBuffering = true;
        int mTargetBufferSize = 15;
        uint64_t mLastSeq = 0;
        uint64_t mExpectedSeq = 0;
        bool mFirstPacket = true;
        int mPLCCount = 0;
    };
static AudioEngine gAudioEngine;

extern "C" {
JNIEXPORT jint JNICALL Java_com_example_audiobtbridge_NativeBridge_initNative(JNIEnv *env, jobject thiz) {
    gAudioEngine.start();
    return 0;
}
JNIEXPORT void JNICALL Java_com_example_audiobtbridge_NativeBridge_stopNative(JNIEnv *env, jobject thiz) {
    gAudioEngine.stop();
}
JNIEXPORT void JNICALL Java_com_example_audiobtbridge_NativeBridge_setBufferSize(JNIEnv *env, jobject thiz, jint size) {
    gAudioEngine.setBufferSize(size);
}
JNIEXPORT jint JNICALL Java_com_example_audiobtbridge_NativeBridge_getBufferDepth(JNIEnv *env, jobject thiz) {
    return gAudioEngine.getBufferDepth();
}
JNIEXPORT jint JNICALL Java_com_example_audiobtbridge_NativeBridge_getPLCCount(JNIEnv *env, jobject thiz) {
    return gAudioEngine.getPLCCount();
}
JNIEXPORT jlong JNICALL Java_com_example_audiobtbridge_NativeBridge_getLastSequence(JNIEnv *env, jobject thiz) {
    return (jlong)gAudioEngine.getLastSequence();
}
JNIEXPORT void JNICALL Java_com_example_audiobtbridge_NativeBridge_writeToNativeBuffer(JNIEnv *env, jobject thiz, jbyteArray data, jint length) {
    jbyte* buffer = env->GetByteArrayElements(data, nullptr);
    if (length >= 8) {
        uint64_t seq = 0;
        memcpy(&seq, buffer, 8);
        gAudioEngine.pushPacket(seq, reinterpret_cast<const uint8_t*>(buffer + 8), (int)length - 8);
    }
    env->ReleaseByteArrayElements(data, buffer, JNI_ABORT);
}
}