package com.example.audiobtbridge

object NativeBridge {
    init {
        System.loadLibrary("audiobtbridge")
    }

    external fun initNative(): Int
    external fun stopNative()
    external fun setBufferSize(size: Int)
    external fun getBufferDepth(): Int
    external fun getLastSequence(): Long
    external fun writeToNativeBuffer(data: ByteArray, length: Int)
}
