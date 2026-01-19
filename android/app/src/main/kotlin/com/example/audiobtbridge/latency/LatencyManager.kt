package com.example.audiobtbridge.latency

enum class LatencyMode {
    LOW_LATENCY,
    BALANCED,
    STABLE
}

data class AudioConfig(
    val bufferSize: Int,
    val bitrate: Int,
    val complexity: Int
)

class LatencyManager {
    fun getConfigForMode(mode: LatencyMode): AudioConfig {
        return when (mode) {
            LatencyMode.LOW_LATENCY -> AudioConfig(
                bufferSize = 2,
                bitrate = 64000,
                complexity = 0
            )
            LatencyMode.BALANCED -> AudioConfig(
                bufferSize = 6,
                bitrate = 128000,
                complexity = 5
            )
            LatencyMode.STABLE -> AudioConfig(
                bufferSize = 20,
                bitrate = 320000,
                complexity = 10
            )
        }
    }
}