package com.example.audiobtbridge

/**
 * Receives retransmission requests from the native decode thread.
 *
 * The decode thread discovers a missing media tick before the audio callback
 * does, so asking for it from there is the earliest possible point. The
 * implementation must be cheap and non-blocking: it runs on the decode thread.
 */
fun interface ResendRequester {
    fun onResendRequest(tick: Long)
}

object NativeBridge {
    init {
        System.loadLibrary("audiobtbridge")
    }

    external fun initNative(): Int
    external fun stopNative()
    external fun resetAudio()
    external fun setBufferSize(size: Int)
    external fun setPlcEnabled(enabled: Boolean)
    external fun updateAdvancedSettings(pcmTarget: Int, catchUpThreshold: Int)
    external fun getBufferDepth(): Int
    external fun getPLCCount(): Int
    external fun getUnderflowCount(): Int
    external fun getDiscardedFrames(): Int
    external fun getMaxBufferedFrames(): Int
    external fun getTargetFrames(): Int
    external fun getCeilingFrames(): Int
    external fun getLatePercentX100(): Int
    external fun writeToNativeBuffer(data: ByteArray, length: Int)

    /**
     * Installs the retransmission callback.
     *
     * The name and signature must match what `native-lib.cpp` looks up with
     * `GetMethodID(cls, "onResendRequest", "(J)V")` — the interface method is
     * compiled to exactly that descriptor.
     */
    external fun setResendCallback(callback: ResendRequester?)
}
