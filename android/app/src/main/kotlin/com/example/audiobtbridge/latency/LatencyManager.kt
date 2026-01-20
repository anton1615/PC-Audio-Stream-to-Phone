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
                bufferSize = 2,      // ~20ms (1 pkt) - 極限低延遲
                bitrate = 128000,
                complexity = 5
            )
            LatencyMode.BALANCE -> AudioConfig(
                bufferSize = 3,      // ~60ms (3 pkts) - 平衡
                bitrate = 19600,
                complexity = 8
            )
            LatencyMode.HIGH_QUALITY -> AudioConfig(
                bufferSize = 7,      // ~140ms (7 pkts) - 高品質
                bitrate = 256000,
                complexity = 10
            )
            LatencyMode.BEST_QUALITY -> AudioConfig(
                bufferSize = 13,     // ~260ms (13 pkts) - 最穩定
                bitrate = 320000,    // Opus 透明音質上限
                complexity = 10
            )
        }
    }
}


