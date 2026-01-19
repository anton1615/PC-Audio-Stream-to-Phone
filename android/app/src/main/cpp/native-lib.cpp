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

class AudioEngine : public oboe::AudioStreamDataCallback {
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
        std::lock_guard<std::mutex> lock(mBufferMutex);
        if (mStream) return;

        // 清空舊緩衝區，防止重啟後聽到舊聲音
        mJitterBuffer.clear();
        mDecodedPcmBuffer.clear();
        mIsBuffering = true;

        oboe::AudioStreamBuilder builder;
        builder.setDirection(oboe::Direction::Output)
               ->setPerformanceMode(oboe::PerformanceMode::LowLatency)
               ->setSharingMode(oboe::SharingMode::Exclusive)
               ->setFormat(oboe::AudioFormat::Float)
               ->setChannelCount(CHANNELS)
               ->setSampleRate(SAMPLE_RATE)
               ->setDataCallback(this);

        oboe::Result result = builder.openStream(mStream);
        if (result != oboe::Result::OK) {
            LOGE("Failed to open stream: %s", oboe::convertToText(result));
            return;
        }

        result = mStream->requestStart();
        if (result != oboe::Result::OK) {
            LOGE("Failed to start stream: %s", oboe::convertToText(result));
            mStream->close();
            mStream.reset();
        }
    }

    void stop() {
        if (mStream) {
            mStream->stop();
            mStream->close();
            mStream.reset();
        }
    }

    oboe::DataCallbackResult onAudioReady(oboe::AudioStream *audioStream, void *audioData, int32_t numFrames) override {
        float *output = static_cast<float *>(audioData);
        int32_t totalSamplesNeeded = numFrames * CHANNELS;
        memset(output, 0, totalSamplesNeeded * sizeof(float));

        std::lock_guard<std::mutex> lock(mBufferMutex);
        
        int threshold = std::max(2, mTargetBufferSize);
        if (mIsBuffering) {
            if (mJitterBuffer.size() >= (size_t)threshold) {
                mIsBuffering = false;
                LOGI("Buffering complete, starting playback");
            } else {
                return oboe::DataCallbackResult::Continue;
            }
        }

        while (mDecodedPcmBuffer.size() < totalSamplesNeeded && !mJitterBuffer.empty()) {
            auto it = mJitterBuffer.begin();
            mLastSeq = it->first; // 更新為正在解碼的包序號
            
            float decodeOut[MAX_FRAME_SIZE * CHANNELS];
            int decodedFrames = opus_decode_float(mOpusDecoder, it->second.data(), it->second.size(), decodeOut, MAX_FRAME_SIZE, 0);

            if (decodedFrames > 0) {
                mDecodedPcmBuffer.insert(mDecodedPcmBuffer.end(), decodeOut, decodeOut + (decodedFrames * CHANNELS));
            }
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
        bool shrinking = size < mTargetBufferSize;
        mTargetBufferSize = size;
        
        if (shrinking) {
            while (mJitterBuffer.size() > (size_t)mTargetBufferSize) {
                mJitterBuffer.erase(mJitterBuffer.begin());
            }
            mIsBuffering = true; 
            LOGI("Buffer shrunk, force resyncing...");
        }
        LOGI("Buffer size set to %d", mTargetBufferSize);
    }

    int getBufferDepth() {
        std::lock_guard<std::mutex> lock(mBufferMutex);
        return (int)mJitterBuffer.size();
    }

    uint64_t getLastSequence() {
        return mLastSeq;
    }

    void pushPacket(uint64_t seq, const uint8_t* data, int len) {
        std::lock_guard<std::mutex> lock(mBufferMutex);
        mJitterBuffer[seq] = std::vector<uint8_t>(data, data + len);
        mLastSeq = seq;
        
        if (mJitterBuffer.size() > (size_t)mTargetBufferSize) {
            mJitterBuffer.erase(mJitterBuffer.begin());
        }
    }

private:
    std::shared_ptr<oboe::AudioStream> mStream;
    std::map<uint64_t, std::vector<uint8_t>> mJitterBuffer;
    std::vector<float> mDecodedPcmBuffer;
    std::mutex mBufferMutex;
    OpusDecoder *mOpusDecoder = nullptr;
    bool mIsBuffering = true; 
    int mTargetBufferSize = 15; 
    uint64_t mLastSeq = 0;
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