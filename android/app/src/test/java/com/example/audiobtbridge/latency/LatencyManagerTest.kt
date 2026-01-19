package com.example.audiobtbridge.latency

import org.junit.Assert.assertEquals
import org.junit.Test

class LatencyManagerTest {

    @Test
    fun `test low latency mode returns small buffer size`() {
        val manager = LatencyManager()
        val bufferSize = manager.getBufferSizeForMode(LatencyMode.LOW)
        // 假設 Low Latency = 3 packets (approx 60ms)
        assertEquals(3, bufferSize)
    }

    @Test
    fun `test balanced latency mode returns medium buffer size`() {
        val manager = LatencyManager()
        val bufferSize = manager.getBufferSizeForMode(LatencyMode.BALANCED)
        // 假設 Balanced = 6 packets (approx 120ms)
        assertEquals(6, bufferSize)
    }

    @Test
    fun `test high stability mode returns large buffer size`() {
        val manager = LatencyManager()
        val bufferSize = manager.getBufferSizeForMode(LatencyMode.HIGH_STABILITY)
        // 假設 High Stability = 15 packets (approx 300ms)
        assertEquals(15, bufferSize)
    }
}
