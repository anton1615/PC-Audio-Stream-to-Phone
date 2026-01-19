package com.example.audiobtbridge

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ConnectionManagerTest {

    @Test
    fun `should send hello if no packets received for 5 seconds`() {
        val manager = ConnectionManager()
        val currentTime = 10000L
        val lastPacketTime = 4000L // 6 seconds ago
        
        assertTrue(manager.shouldSendHello(currentTime, lastPacketTime))
    }

    @Test
    fun `should NOT send hello if packet received recently`() {
        val manager = ConnectionManager()
        val currentTime = 10000L
        val lastPacketTime = 9000L // 1 second ago
        
        assertFalse(manager.shouldSendHello(currentTime, lastPacketTime))
    }
}
