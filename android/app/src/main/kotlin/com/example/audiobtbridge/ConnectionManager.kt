package com.example.audiobtbridge

class ConnectionManager {
    private var lastSequence: Long = -1

    fun shouldSendHello(currentTime: Long, lastPacketTime: Long): Boolean {
        return (currentTime - lastPacketTime) > 5000
    }

    fun isDuplicate(seq: Long): Boolean {
        if (seq == lastSequence) return true
        lastSequence = seq
        return false
    }

    fun reset() {
        lastSequence = -1
    }
}
