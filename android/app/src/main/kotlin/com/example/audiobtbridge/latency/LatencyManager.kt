package com.example.audiobtbridge.latency

enum class LatencyMode {
    LOW_LATENCY,
    BALANCE,
    HIGH_QUALITY,
    BEST_QUALITY
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
                bufferSize = 2,      // ~40ms
                bitrate = 96000,
                complexity = 5
            )
            LatencyMode.BALANCE -> AudioConfig(
                bufferSize = 4,      // ~80ms
                bitrate = 196000,
                complexity = 8
            )
            LatencyMode.HIGH_QUALITY -> AudioConfig(
                bufferSize = 6,     // ~120ms
                bitrate = 256000,
                complexity = 10
            )
            LatencyMode.BEST_QUALITY -> AudioConfig(
                bufferSize = 8,     // ~160ms
                bitrate = 320000,    // Transparent
                complexity = 10
            )
        }
    }
}