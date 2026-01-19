package com.example.audiobtbridge

class ConnectionManager {
    fun shouldSendHello(currentTime: Long, lastPacketTime: Long): Boolean {
        return (currentTime - lastPacketTime) > 5000
    }
}
