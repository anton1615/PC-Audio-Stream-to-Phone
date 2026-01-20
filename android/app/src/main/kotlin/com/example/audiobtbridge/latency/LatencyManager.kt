package com.example.audiobtbridge.latency

enum class LatencyMode {
    LOW_LATENCY,
    BALANCED,
    HIGH_QUALITY
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
                bufferSize = 10, // 從 6 提升到 10，增加背景穩定性
                bitrate = 128000,
                complexity = 5
            )
            LatencyMode.HIGH_QUALITY -> AudioConfig(
                bufferSize = 20, // 改回原來的 20
                bitrate = 320000,
                complexity = 10
            )
        }
    }
}