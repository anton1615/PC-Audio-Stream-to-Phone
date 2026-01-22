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

    @Test
    fun `should detect duplicate sequence number`() {
        val manager = ConnectionManager()
        
        assertFalse(manager.isDuplicate(100L)) // First time: not a duplicate
        assertTrue(manager.isDuplicate(100L))  // Second time: duplicate
        assertFalse(manager.isDuplicate(101L)) // New sequence: not a duplicate
    }

    @Test
    fun `should signal stop when Bluetooth disconnects`() {
        val manager = ConnectionManager()
        assertTrue(manager.shouldStopOnBluetoothDisconnect(isConnected = true, isBluetoothConnected = false))
    }

    @Test
    fun `should NOT signal stop when Bluetooth connects`() {
        val manager = ConnectionManager()
        assertFalse(manager.shouldStopOnBluetoothDisconnect(isConnected = true, isBluetoothConnected = true))
    }

    @Test
    fun `should NOT signal stop when not streaming`() {
        val manager = ConnectionManager()
        assertFalse(manager.shouldStopOnBluetoothDisconnect(isConnected = false, isBluetoothConnected = false))
    }
}
