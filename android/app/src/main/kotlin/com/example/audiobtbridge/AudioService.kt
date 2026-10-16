package com.example.audiobtbridge

import android.app.*
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.bluetooth.BluetoothDevice
import android.media.AudioDeviceCallback
import android.media.AudioDeviceInfo
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.net.wifi.WifiManager
import android.os.*
import android.support.v4.media.session.MediaSessionCompat
import android.support.v4.media.session.PlaybackStateCompat
import android.util.Log
import androidx.core.app.NotificationCompat
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.net.BindException
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetAddress
import java.net.NetworkInterface
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.atomic.AtomicInteger
import com.example.audiobtbridge.latency.LatencyManager
import com.example.audiobtbridge.latency.LatencyMode
import com.example.audiobtbridge.latency.AudioConfig

class AudioService : Service() {

    private val serviceScope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private var isRunning = false
    private var udpSocket: DatagramSocket? = null
    private var serverAddress: InetAddress? = null
    private val latencyManager = LatencyManager()
    private val connectionManager = ConnectionManager()
    private lateinit var audioManager: AudioManager
    private var mediaSession: MediaSessionCompat? = null
    private var currentActiveConfig: AudioConfig? = null
    private var isUiVisible = false
    private var lastNotificationContent: String? = null
    private var currentNotification: Notification? = null

    // Power management. The CPU must stay awake while streaming, and the Wi-Fi
    // stack drops multicast/broadcast frames unless a MulticastLock is held —
    // without it the discovery packets stop arriving once the screen turns off.
    private var wakeLock: PowerManager.WakeLock? = null
    private var multicastLock: WifiManager.MulticastLock? = null
    private var audioFocusRequest: AudioFocusRequest? = null

    // Set while the service is streaming, so the native resend callback can find
    // the socket and server address without racing against onDestroy.
    @Volatile private var resendSocket: DatagramSocket? = null
    @Volatile private var resendTarget: InetAddress? = null
    private val pendingResendTicks = AtomicInteger(0)
    
    companion object {
        private val _serviceState = MutableStateFlow(false)
        val serviceState = _serviceState.asStateFlow()

        private val _latencyModeFlow = MutableStateFlow(LatencyMode.BALANCE)
        val latencyModeFlow = _latencyModeFlow.asStateFlow()

        private val _latencyFlow = MutableStateFlow(0.0f)
        val latencyFlow = _latencyFlow.asStateFlow()

        private val _latencyHistoryFlow = MutableStateFlow<List<Float>>(emptyList())
        val latencyHistoryFlow = _latencyHistoryFlow.asStateFlow()

        private val _isSearchingFlow = MutableStateFlow(false)
        val isSearchingFlow = _isSearchingFlow.asStateFlow()

        const val ACTION_START = "ACTION_START"
        const val ACTION_STOP = "ACTION_STOP"
        const val ACTION_UPDATE_MODE = "ACTION_UPDATE_MODE"
        const val ACTION_UPDATE_PLC = "ACTION_UPDATE_PLC"
        const val ACTION_UPDATE_ADVANCED = "ACTION_UPDATE_ADVANCED"
        const val ACTION_UI_VISIBLE = "ACTION_UI_VISIBLE"
        const val ACTION_UI_HIDDEN = "ACTION_UI_HIDDEN"
        const val EXTRA_MODE = "EXTRA_MODE"
        const val EXTRA_PLC = "EXTRA_PLC"
        const val EXTRA_BITRATE = "EXTRA_BITRATE"
        const val EXTRA_COMPLEXITY = "EXTRA_COMPLEXITY"
        const val EXTRA_BUFFER_SIZE = "EXTRA_BUFFER_SIZE"
        const val EXTRA_PCM_TARGET = "EXTRA_PCM_TARGET"
        const val EXTRA_CATCHUP = "EXTRA_CATCHUP"
        private const val CHANNEL_ID = "audio_stream_channel"

        // Server packet header (v9), measured after the 10-byte "AS2P_AUDIO" prefix.
        // tick(8) + send_ts_ms(8) + flags(8) + seq(4)
        const val AUDIO_HEADER_LEN = 28 + 10
        private const val PACKET_HEADER_LEN = 28
        private const val FLAG_RETRANSMIT = 1L

        // Client -> server: "AS2P_RESEND" + tick(8)
        private const val RESEND_PREFIX = "AS2P_RESEND"
        // Client -> server: "AS2P_STATS" + 5 x u32 (max latency ms, avg latency ms,
        // late percent x100, loss percent x100, underflow count)
        private const val STATS_PREFIX = "AS2P_STATS"
        private const val STATS_INTERVAL_MS = 1000L
        private const val FRAME_MS = 20.0f

        /** Cap on concurrently outstanding resend requests. */
        private const val MAX_INFLIGHT_RESENDS = 16

        fun setModeOffline(mode: LatencyMode) { _latencyModeFlow.value = mode }
        
        fun updatePlcEnabled(enabled: Boolean) {
            serviceInstance?.let {
                val intent = Intent(it, AudioService::class.java).apply {
                    action = ACTION_UPDATE_PLC
                    putExtra(EXTRA_PLC, enabled)
                }
                it.startService(intent)
            }
        }

        fun applyAdvancedSettings(bitrate: Int, complexity: Int, bufferSize: Int, pcmTarget: Int, catchup: Int) {
            serviceInstance?.let {
                val intent = Intent(it, AudioService::class.java).apply {
                    action = ACTION_UPDATE_ADVANCED
                    putExtra(EXTRA_BITRATE, bitrate)
                    putExtra(EXTRA_COMPLEXITY, complexity)
                    putExtra(EXTRA_BUFFER_SIZE, bufferSize)
                    putExtra(EXTRA_PCM_TARGET, pcmTarget)
                    putExtra(EXTRA_CATCHUP, catchup)
                }
                it.startService(intent)
            }
        }
        
        // Internal reference to the service instance for notification updates
        private var serviceInstance: AudioService? = null

        fun log(msg: String) {
            Log.i("AS2P_Service", msg)
            // Only update notification for non-stats messages to avoid spamming the UI with detailed logs
            // unless it is a state change we want to show.
            if (!msg.startsWith("[Stats]")) {
                serviceInstance?.updateNotification(msg)
            }
        }

        fun updateNotificationState(content: String) {
            serviceInstance?.updateNotification(content)
        }
    }

    private fun updateNotification(content: String) {
        // Power Optimization: Skip update if content is identical
        if (content == lastNotificationContent) return
        lastNotificationContent = content

        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        
        // 使用 MediaStyle
        val mediaStyle = androidx.media.app.NotificationCompat.MediaStyle()
            .setMediaSession(mediaSession?.sessionToken)
            .setShowActionsInCompactView(0) // 顯示第一個按鈕 (Stop)

        val stopIntent = Intent(this, AudioService::class.java).apply { action = ACTION_STOP }
        val stopPendingIntent = PendingIntent.getService(this, 0, stopIntent, PendingIntent.FLAG_IMMUTABLE)

        // Clean up content text: Remove Redundancy/PLC/Stats info
        // Format: "[Preset] - [Status]"
        val cleanContent = if (content.contains("Redundancy")) {
            val modeName = _latencyModeFlow.value.name.replace("_", " ")
            "$modeName - Streaming"
        } else {
            content
        }

        val notification = NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle("AS2P Audio Bridge")
            .setContentText(cleanContent)
            .setSmallIcon(android.R.drawable.ic_media_play) // Status bar icon
            .setLargeIcon(android.graphics.BitmapFactory.decodeResource(resources, R.drawable.app_icon)) // Album art
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .setOngoing(isRunning)
            .setStyle(mediaStyle)
            .addAction(R.drawable.ic_stop, "Stop", stopPendingIntent) // Custom Stop Icon
            .build()

        nm.notify(1, notification)
        currentNotification = notification
    }

    /**
     * Puts the service in the foreground with the notification we last built.
     * Must be called once for every startForegroundService() request, see the
     * ACTION_START branch in onStartCommand().
     */
    private fun promoteToForeground(notification: Notification) {
        currentNotification = notification
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(1, notification, android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK)
        } else startForeground(1, notification)
    }

    private val bluetoothReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            val action = intent?.action
            if (BluetoothDevice.ACTION_ACL_DISCONNECTED == action || AudioManager.ACTION_AUDIO_BECOMING_NOISY == action) {
                if (isRunning) {
                    log("Audio Device Disconnected. Terminating Service...")
                    // 1. Send Goodbye if connected
                    if (serverAddress != null) {
                        sendGoodbyeToServer()
                    }
                    // 2. Stop Service
                    stopSelf()
                }
            }
        }
    }

    override fun onCreate() {
        super.onCreate()
        serviceInstance = this
        audioManager = getSystemService(Context.AUDIO_SERVICE) as AudioManager
        createNotificationChannel()
        
        // Register Bluetooth and Audio Noisy monitors
        val filter = IntentFilter().apply {
            addAction(BluetoothDevice.ACTION_ACL_DISCONNECTED)
            addAction(AudioManager.ACTION_AUDIO_BECOMING_NOISY)
        }
        registerReceiver(bluetoothReceiver, filter)
        
        // Listen for audio device changes

        audioManager.registerAudioDeviceCallback(object : AudioDeviceCallback() {
            override fun onAudioDevicesAdded(addedDevices: Array<out AudioDeviceInfo>?) {
                if (isRunning) {
                    log("Audio Device Added: Resetting Buffer...")
                    NativeBridge.resetAudio()
                    // Re-sync current config to server
                    currentActiveConfig?.let { sendConfigToServer(it) }
                }
            }
            override fun onAudioDevicesRemoved(removedDevices: Array<out AudioDeviceInfo>?) {
                if (isRunning) {
                    log("Audio Device Removed: Resetting Buffer...")
                    NativeBridge.resetAudio()
                    // Re-sync current config to server
                    currentActiveConfig?.let { sendConfigToServer(it) }
                }
            }
        }, null)

        mediaSession = MediaSessionCompat(this, "AS2P").apply {
            setPlaybackState(PlaybackStateCompat.Builder().setState(PlaybackStateCompat.STATE_PLAYING, 0, 1.0f).build())
            isActive = true
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_START -> if (!isRunning) {
                // stopSelf() tears the service instance down, so a reconnect runs on a brand
                // new instance whose isUiVisible is back to its false default. ACTION_START
                // only ever comes from the CONNECT button, so the UI is on screen by
                // definition here — re-assert it, otherwise the stats loop never runs and
                // the latency readout stays at 0.
                isUiVisible = true
                startAudioService()
            } else {
                // Already streaming. A second ACTION_START still reaches us whenever the
                // CONNECT button is tapped twice before Compose recomposes, and Android
                // requires a startForeground() for *every* startForegroundService() call:
                // skipping it crashes the app ~5s later with
                // ForegroundServiceDidNotStartInTimeException. Re-asserting the
                // foreground state is harmless when it is already foreground.
                currentNotification?.let { promoteToForeground(it) }
            }
            ACTION_STOP -> {
                // Do not clear serverAddress here, as it's needed by sendGoodbyeToServer() in onDestroy()
                //
                // A stop can also be delivered through startForegroundService(), which creates a
                // fresh instance that has never been foreground. Android enforces the
                // startForeground() contract per request and tears such a service down with
                // "Bringing down service while still waiting for start foreground", then kills
                // the app ~5s later with ForegroundServiceDidNotStartInTimeException. Satisfy
                // the contract before stopping.
                if (!isRunning) {
                    updateNotification("Stopping...")
                    currentNotification?.let { promoteToForeground(it) }
                }
                stopSelf()
            }
            ACTION_UPDATE_MODE -> {
                val modeName = intent.getStringExtra(EXTRA_MODE)
                modeName?.let { updateLatencyMode(LatencyMode.valueOf(it)) }
            }
            ACTION_UPDATE_PLC -> {
                val enabled = intent.getBooleanExtra(EXTRA_PLC, true)
                NativeBridge.setPlcEnabled(enabled)
                log("PLC ${if (enabled) "Enabled" else "Disabled"}")
            }
            ACTION_UPDATE_ADVANCED -> {
                val br = intent.getIntExtra(EXTRA_BITRATE, 160000)
                val comp = intent.getIntExtra(EXTRA_COMPLEXITY, 8)
                val buf = intent.getIntExtra(EXTRA_BUFFER_SIZE, 4)
                val pcmT = intent.getIntExtra(EXTRA_PCM_TARGET, 2)
                val cup = intent.getIntExtra(EXTRA_CATCHUP, 1)
                
                NativeBridge.setBufferSize(buf)
                NativeBridge.updateAdvancedSettings(pcmT, cup)
                sendConfigToServer(AudioConfig(buf, br, comp))
                log("Advanced Settings Applied")
            }
            ACTION_UI_VISIBLE -> isUiVisible = true
            ACTION_UI_HIDDEN -> isUiVisible = false
        }
        return START_NOT_STICKY
    }

    private fun startAudioService() {
        createNotificationChannel()

        // Initial MediaStyle Notification
        val mediaStyle = androidx.media.app.NotificationCompat.MediaStyle()
            .setMediaSession(mediaSession?.sessionToken)
            .setShowActionsInCompactView(0)

        val stopIntent = Intent(this, AudioService::class.java).apply { action = ACTION_STOP }
        val stopPendingIntent = PendingIntent.getService(this, 0, stopIntent, PendingIntent.FLAG_IMMUTABLE)

        val notification = NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle("AS2P Audio Bridge")
            .setContentText("Searching for server...")
            .setSmallIcon(android.R.drawable.ic_media_play)
            .setLargeIcon(android.graphics.BitmapFactory.decodeResource(resources, R.drawable.app_icon))
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .setOngoing(true)
            .setStyle(mediaStyle)
            .addAction(R.drawable.ic_stop, "Stop", stopPendingIntent)
            .build()
            
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(1, notification, android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK)
        } else startForeground(1, notification)
        currentNotification = notification
        
        isRunning = true
        _serviceState.value = true
        _isSearchingFlow.value = true

        // Keep the CPU and the Wi-Fi multicast filter awake for the whole session.
        acquirePowerLocks()
        if (!requestAudioFocus()) {
            log("Audio focus denied. Stopping...")
            stopSelf()
            return
        }

        // Dedicated socket for resend requests and stats. The receiver socket is
        // parked in a blocking receive() and must not be shared.
        try {
            resendSocket = DatagramSocket()
        } catch (e: Exception) {
            Log.w("AS2P_Service", "Resend socket unavailable: ${e.message}")
            resendSocket = null
        }

        NativeBridge.initNative()
        NativeBridge.setResendCallback(ResendRequester { tick -> requestResend(tick) })
        
        // --- Sync Settings to Native layer ---
        val prefs = getSharedPreferences("AS2P_Prefs", Context.MODE_PRIVATE)
        val mode = _latencyModeFlow.value
        val config = latencyManager.getConfigForMode(mode)
        
        // Load advanced overrides if present, otherwise use mode defaults
        val buf = prefs.getInt("adv_buffer_size", config.bufferSize)
        val bitrate = prefs.getInt("adv_bitrate", config.bitrate)
        val complexity = prefs.getInt("adv_complexity", config.complexity)
        val pcmT = prefs.getInt("adv_pcm_target", 5) // Default to legacy behavior if not set
        val cup = prefs.getInt("adv_catchup", 1)
        val plc = prefs.getBoolean("plc_enabled", true)

        NativeBridge.setBufferSize(buf)
        NativeBridge.updateAdvancedSettings(pcmT, cup)
        NativeBridge.setPlcEnabled(plc)
        
        currentActiveConfig = AudioConfig(buf, bitrate, complexity)
        Log.i("AS2P_Diag", "[Init] Synced Settings: Buf=$buf, BR=$bitrate, Cmp=$complexity, PCM=$pcmT, Catchup=+$cup, PLC=$plc")

        // --- Config & Audio Watchdog ---
        var lastAudioTime = System.currentTimeMillis()
        var configRetryCount = 0
        serviceScope.launch {
            while (isRunning) {
                val currentTime = System.currentTimeMillis()
                if (serverAddress != null) {
                    // 如果已連線但超過 1.5 秒沒收到音訊
                    if (currentTime - lastAudioTime > 1500) {
                                                    if (configRetryCount < 3) {
                                                        log("No audio received. Retrying Config (${++configRetryCount}/3)...")
                                                        mediaSession?.setPlaybackState(PlaybackStateCompat.Builder().setState(PlaybackStateCompat.STATE_BUFFERING, 0, 1.0f).build())
                                                        currentActiveConfig?.let { sendConfigToServer(it) }
                                                        lastAudioTime = currentTime // 給 Server 一點反應時間
                                                    } else {
                                                        log("Connection lost (Timeout). Back to searching...")
                                                        mediaSession?.setPlaybackState(PlaybackStateCompat.Builder().setState(PlaybackStateCompat.STATE_PAUSED, 0, 1.0f).build())
                                                        clearServerConnection()
                                                        _isSearchingFlow.value = true
                                                        configRetryCount = 0
                                                    }
                        
                    }
                }
                delay(500)
            }
        }
        
        // Stats Thread
        serviceScope.launch {
            while (isRunning) {
                if (isUiVisible) {
                    val latency = NativeBridge.getBufferDepth() * FRAME_MS
                    _latencyFlow.value = latency
                    val history = _latencyHistoryFlow.value.toMutableList()
                    history.add(latency)
                    if (history.size > 60) history.removeAt(0)
                    _latencyHistoryFlow.value = history
                }
                delay(100)
            }
        }

        // Client-side link report. The server has no way to observe latency, loss
        // or underruns on this side of the link.
        serviceScope.launch(Dispatchers.IO) {
            while (isRunning) {
                if (resendTarget != null) sendStatsToServer()
                delay(STATS_INTERVAL_MS)
            }
        }

        // Receiver Loop
                var duplicateCount = 0
                var lastLogTime = 0L
                var lastReceivedSeq = -1L

        serviceScope.launch(Dispatchers.IO) {
            val socket = bindReceiverSocket()
            if (socket == null) return@launch
            try {
                val buffer = ByteArray(2048)
                val packet = DatagramPacket(buffer, buffer.size)
                
                log("Service Started. Listening on UDP 12345...")

                while (isRunning) {
                    socket.soTimeout = 3000
                    try {
                        socket.receive(packet)
                        val len = packet.length
                        val data = packet.data
                        
                        // V8 Protocol Handling
                        if (len >= 10 && String(data, 0, 10) == "AS2P_OFFER") {
                            if (serverAddress == null) {
                                serverAddress = packet.address
                                resendTarget = packet.address
                                Log.i("AS2P_Diag", "[Conn] Server Address set to: ${packet.address}")
                                _isSearchingFlow.value = false
                                mediaSession?.setPlaybackState(PlaybackStateCompat.Builder().setState(PlaybackStateCompat.STATE_PLAYING, 0, 1.0f).build())
                                log("Server Found: ${packet.address}")
                                NativeBridge.resetAudio()
                                connectionManager.reset()
                                lastReceivedSeq = -1L // Reset tracker
                                
                                // Fix: Explicitly apply current mode settings to Native & Server upon connection
                                updateLatencyMode(_latencyModeFlow.value)
                                
                                lastAudioTime = System.currentTimeMillis()
                                configRetryCount = 0
                            }
                        } else if (len > 10 && String(data, 0, 10) == "AS2P_AUDIO") {
                            _isSearchingFlow.value = false 
                            lastAudioTime = System.currentTimeMillis()
                            configRetryCount = 0

                            // Audio arriving proves the server is streaming, so adopt
                            // its address even if the OFFER broadcast was lost. Without
                            // this, a missed OFFER would leave resendTarget null and
                            // silently disable retransmission for the whole session.
                            if (serverAddress == null) {
                                serverAddress = packet.address
                                resendTarget = packet.address
                                _isSearchingFlow.value = false
                                mediaSession?.setPlaybackState(
                                    PlaybackStateCompat.Builder()
                                        .setState(PlaybackStateCompat.STATE_PLAYING, 0, 1.0f)
                                        .build()
                                )
                                Log.i("AS2P_Diag", "[Conn] Server inferred from audio: ${packet.address}")
                            }
                            
                            if (len > AUDIO_HEADER_LEN) {
                                // v9 header after the 10-byte prefix:
                                // tick(8) + send_ts_ms(8) + flags(8) + seq(4)
                                val hdr = ByteBuffer.wrap(data, 10, PACKET_HEADER_LEN).order(ByteOrder.LITTLE_ENDIAN)
                                val tick = hdr.long
                                hdr.long   // send_ts_ms: informational, not needed for playback
                                val flags = hdr.long
                                val seq = hdr.int.toLong() and 0xFFFFFFFFL

                                val isRetransmit = (flags and FLAG_RETRANSMIT) != 0L

                                if (lastReceivedSeq != -1L && seq > lastReceivedSeq + 1) {
                                    Log.w("AS2P_Diag", "[Network] Gap: ${lastReceivedSeq + 1} to ${seq - 1}")
                                }
                                lastReceivedSeq = seq

                                // Retransmissions legitimately repeat a tick, so they must not
                                // be treated as duplicates of the original.
                                if (!isRetransmit && connectionManager.isDuplicate(seq)) {
                                    duplicateCount++
                                    continue
                                }

                                val now = System.currentTimeMillis()
                                if (now - lastLogTime > 2000) {
                                    val plcCount = NativeBridge.getPLCCount()
                                    val underflows = NativeBridge.getUnderflowCount()
                                    val discarded = NativeBridge.getDiscardedFrames()
                                    if (duplicateCount > 0 || plcCount > 0 || underflows > 0 || discarded > 0) {
                                        // Counters are cumulative since the last reset, not
                                        // per-interval.
                                        Log.i("AS2P_Service", "[Stats] Dupes: $duplicateCount (2s), PLC: $plcCount, Underflows: $underflows, Discarded: $discarded (session)")
                                        val modeName = _latencyModeFlow.value.name.replace("_", " ")
                                        updateNotificationState("$modeName - Streaming")
                                    }
                                    duplicateCount = 0
                                    lastLogTime = now
                                }

                                // The native layer parses the header itself (it needs
                                // the tick to key its jitter buffer), so hand it
                                // everything after the "AS2P_AUDIO" prefix.
                                val payloadSize = len - 10
                                if (payloadSize > PACKET_HEADER_LEN) {
                                    val payload = ByteArray(payloadSize)
                                    System.arraycopy(data, 10, payload, 0, payloadSize)
                                    NativeBridge.writeToNativeBuffer(payload, payloadSize)
                                }
                            }
                        } else if (len >= 12 && String(data, 0, 12) == "AS2P_GOODBYE") {
                            log("Server Stopped. Resetting...")
                            mediaSession?.setPlaybackState(PlaybackStateCompat.Builder().setState(PlaybackStateCompat.STATE_PAUSED, 0, 1.0f).build())
                            clearServerConnection()
                            _isSearchingFlow.value = true
                            NativeBridge.resetAudio()
                            connectionManager.reset()
                        }
                    } catch (e: java.net.SocketTimeoutException) {
                        if (serverAddress != null) {
                            log("Connection Timeout. Searching...")
                            clearServerConnection()
                            _isSearchingFlow.value = true
                        }
                    } catch (e: Exception) {
                        if (isRunning) {
                            log("Error: ${e.message}")
                        } else {
                            // Service is stopping, socket close is expected.
                            Log.i("AS2P_Service", "Socket closed gracefully.")
                        }
                    }
                }
            } finally {
                socket.close()
                if (udpSocket === socket) udpSocket = null
            }
        }

        // Heartbeat Loop
        serviceScope.launch(Dispatchers.IO) {
            while (isRunning) {
                serverAddress?.let { addr ->
                    try {
                        val msg = "AS2P_ALIVE".toByteArray()
                        val packet = DatagramPacket(msg, msg.size, addr, 12345)
                        udpSocket?.send(packet)
                    } catch (e: Exception) {}
                }
                delay(2000)
            }
        }

        // Discovery Loop
        serviceScope.launch(Dispatchers.IO) {
            val msg = "AS2P_DISCOVER".toByteArray()
            log("Discovery Loop Started (Interval: 1000ms)")

            while (isRunning) {
                if (serverAddress == null) {
                    broadcastDiscovery(msg)
                }
                delay(1000)
            }
        }
    }

    /**
     * Binds the UDP receiver socket, waiting out a port that a previous service
     * instance has not released yet.
     *
     * A rapid STOP -> CONNECT cycle starts the new service instance while the
     * previous one is still tearing down. Its receiver coroutine is parked in a
     * blocking receive(), and a blocked recvfrom keeps its own reference to the
     * socket, so the kernel does not free port 12345 until that thread unwinds -
     * tens of milliseconds after onDestroy() closed it. A single bind attempt
     * loses that race with "EADDRINUSE", and giving up would leave the service
     * running with no socket at all: no discovery, no audio, and the UI stuck on
     * "SEARCHING FOR SERVER..." until the user toggles again.
     *
     * So keep retrying for as long as the service is meant to be running. The
     * first attempt is immediate and the delay then backs off, so a port that is
     * genuinely taken does not spin.
     *
     * @return the bound socket, or null if the service stopped while waiting.
     */
    private suspend fun bindReceiverSocket(): DatagramSocket? {
        val maxDelayMs = 1000L
        var retryDelayMs = 150L
        var failures = 0
        var lastLogTime = 0L

        while (isRunning) {
            // Drop a socket left over from an earlier start in this same instance.
            udpSocket?.close()
            udpSocket = null
            try {
                val socket = DatagramSocket(12345)
                socket.receiveBufferSize = 1024 * 1024
                udpSocket = socket
                if (failures > 0) log("Port 12345 acquired after $failures retries")
                return socket
            } catch (e: Exception) {
                // Only a taken port is worth waiting for; anything else is a real
                // failure and retrying would just spin on it.
                if (e !is BindException && e.message?.contains("EADDRINUSE") != true) {
                    log("Socket Error: ${e.message}")
                    return null
                }
                failures++
                val now = System.currentTimeMillis()
                if (failures == 1 || now - lastLogTime > 10_000) {
                    log(if (failures == 1) "Port 12345 busy, retrying..."
                        else "Port 12345 still busy, retrying...")
                    lastLogTime = now
                }
                delay(retryDelayMs)
                retryDelayMs = (retryDelayMs * 2).coerceAtMost(maxDelayMs)
            }
        }
        return null
    }

    /**
     * Sends the discovery packet through both discovery paths, so whichever
     * network the PC ends up on is covered automatically:
     *   1. Limited broadcast (255.255.255.255) on the default network.
     *   2. Subnet broadcast on every usable interface.
     * Path 2 is what makes a second network work (e.g. USB tethering): an
     * unbound socket sending to 255.255.255.255 is routed via the default
     * network only, while an interface's own broadcast address follows that
     * interface's route.
     */
    private fun broadcastDiscovery(msg: ByteArray) {
        val targets = LinkedHashSet<InetAddress>()
        try { targets.add(InetAddress.getByName("255.255.255.255")) } catch (e: Exception) {}
        try {
            for (nif in NetworkInterface.getNetworkInterfaces()) {
                if (!nif.isUp || nif.isLoopback) continue
                for (ia in nif.interfaceAddresses) {
                    ia.broadcast?.let { targets.add(it) }
                }
            }
        } catch (e: Exception) {}
        for (addr in targets) {
            try {
                DatagramSocket().use { socket ->
                    socket.broadcast = true
                    socket.send(DatagramPacket(msg, msg.size, addr, 12345))
                }
            } catch (e: Exception) {}
        }
    }

    private fun updateLatencyMode(mode: LatencyMode) {
        _latencyModeFlow.value = mode
        val prefs = getSharedPreferences("AS2P_Prefs", Context.MODE_PRIVATE)
        
        if (mode == LatencyMode.CUSTOM_SETTING) {
            // Load and apply the saved advanced settings
            val br = prefs.getInt("adv_bitrate", 160000)
            val comp = prefs.getInt("adv_complexity", 8)
            val buf = prefs.getInt("adv_buffer_size", 4)
            val pcmT = prefs.getInt("adv_pcm_target", 2)
            val cup = prefs.getInt("adv_catchup", 1)
            val plc = prefs.getBoolean("plc_enabled", true)

            NativeBridge.setBufferSize(buf)
            NativeBridge.updateAdvancedSettings(pcmT, cup)
            NativeBridge.setPlcEnabled(plc)
            sendConfigToServer(AudioConfig(buf, br, comp))
            log("Custom Settings Applied")
        } else {
            // Standard Preset: Use defaults and enforce standard stability logic
            val config = latencyManager.getConfigForMode(mode)
            
            // Force Standard Logic: PCM Target = 2, Catch-up = +1, PLC = Enabled
            NativeBridge.setBufferSize(config.bufferSize)
            NativeBridge.updateAdvancedSettings(2, 1) 
            NativeBridge.setPlcEnabled(true)
            
            sendConfigToServer(config)
            log("Mode Updated: ${mode.name}")
        }
    }

    private fun sendConfigToServer(config: AudioConfig) {
        currentActiveConfig = config
        val target = serverAddress ?: return
        serviceScope.launch(Dispatchers.IO) {
            try {
                val socket = DatagramSocket()
                val prefix = "AS2P_CONFIG".toByteArray()
                val buffer = ByteBuffer.allocate(prefix.size + 5) // Prefix + Bitrate(4) + Complexity(1)
                buffer.put(prefix)
                buffer.order(ByteOrder.LITTLE_ENDIAN)
                buffer.putInt(config.bitrate)
                buffer.put(config.complexity.toByte())
                val data = buffer.array()
                socket.send(DatagramPacket(data, data.size, target, 12345))
                socket.close()
                log("Config Sent: ${config.bitrate}bps, Cmp:${config.complexity}")
            } catch (e: Exception) { log("Config Send Failed: ${e.message}") }
        }
    }

    /**
     * Drops the server association.
     *
     * `resendTarget` must be cleared alongside `serverAddress`: the native
     * decode thread keeps requesting resends as long as it has a target, and a
     * stale one would spray requests at a server that is no longer streaming.
     */
    private fun clearServerConnection() {
        serverAddress = null
        resendTarget = null
    }

    /**
     * Asks the server to resend the packet carrying [tick].
     *
     * Called from the native decode thread the moment a gap is detected, so this
     * must not block. It reuses a dedicated socket instead of the receiver,
     * because the receiver is parked in a blocking `receive()`.
     *
     * A small in-flight budget keeps a long burst of gaps from flooding the
     * server; the native side already caps each gap at 8 requests.
     */
    private fun requestResend(tick: Long) {
        val target = resendTarget ?: return
        val socket = resendSocket ?: return
        if (pendingResendTicks.get() >= MAX_INFLIGHT_RESENDS) return
        pendingResendTicks.incrementAndGet()
        try {
            val packet = ByteBuffer.allocate(RESEND_PREFIX.length + 8)
                .order(ByteOrder.LITTLE_ENDIAN)
                .put(RESEND_PREFIX.toByteArray())
                .putLong(tick)
                .array()
            socket.send(DatagramPacket(packet, packet.size, target, 12345))
            Log.i("AS2P_Diag", "[Net] Resend requested for tick $tick")
        } catch (e: Exception) {
            // Losing a resend request only costs one concealed frame.
            Log.w("AS2P_Diag", "[Net] Resend request failed: ${e.message}")
        } finally {
            pendingResendTicks.decrementAndGet()
        }
    }

    /**
     * Reports the client's own view of the link to the server.
     *
     * None of this is derivable from the server's send path: latency, loss and
     * underruns all happen on the receiving side. Without it there is no way to
     * tell whether the adaptive buffer is helping or just adding delay.
     */
    private fun sendStatsToServer() {
        val target = resendTarget ?: return
        val socket = resendSocket ?: return
        try {
            val depthFrames = NativeBridge.getBufferDepth()
            val maxFrames = NativeBridge.getMaxBufferedFrames()
            val lateX100 = NativeBridge.getLatePercentX100()
            val underflows = NativeBridge.getUnderflowCount()

            val packet = ByteBuffer.allocate(STATS_PREFIX.length + 5 * 4)
                .order(ByteOrder.LITTLE_ENDIAN)
                .put(STATS_PREFIX.toByteArray())
                .putInt(maxFrames * 20)          // max latency ms
                .putInt(depthFrames * 20)        // current latency ms (used as average)
                .putInt(lateX100)                // late packet percent x100
                .putInt(0)                       // packet loss percent x100 (server side counts drops)
                .putInt(underflows)
                .array()
            socket.send(DatagramPacket(packet, packet.size, target, 12345))
        } catch (e: Exception) {
            // Stats are best-effort.
        }
    }

    private fun acquirePowerLocks() {
        if (wakeLock == null) {
            val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
            wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "AS2P::StreamWakeLock").apply {
                setReferenceCounted(false)
                acquire()
            }
        }
        if (multicastLock == null) {
            val wm = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
            // Discovery relies on broadcast/multicast reception, which the Wi-Fi
            // stack filters out once the device dozes unless this lock is held.
            multicastLock = wm.createMulticastLock("AS2P::MulticastLock").apply {
                setReferenceCounted(false)
                acquire()
            }
        }
    }

    private fun releasePowerLocks() {
        wakeLock?.let { if (it.isHeld) it.release() }
        wakeLock = null
        multicastLock?.let { if (it.isHeld) it.release() }
        multicastLock = null
    }

    private fun requestAudioFocus(): Boolean {
        val am = getSystemService(Context.AUDIO_SERVICE) as AudioManager
        val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
            .setAudioAttributes(
                android.media.AudioAttributes.Builder()
                    .setUsage(android.media.AudioAttributes.USAGE_MEDIA)
                    .setContentType(android.media.AudioAttributes.CONTENT_TYPE_MUSIC)
                    .build()
            )
            .setOnAudioFocusChangeListener { change ->
                if (change <= AudioManager.AUDIOFOCUS_LOSS) {
                    Log.i("AS2P_Service", "Audio focus lost ($change); stopping stream")
                    log("Audio focus lost. Stopping...")
                    stopSelf()
                }
            }
            .build()
        audioFocusRequest = request
        return am.requestAudioFocus(request) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
    }

    private fun abandonAudioFocus() {
        audioFocusRequest?.let {
            val am = getSystemService(Context.AUDIO_SERVICE) as AudioManager
            am.abandonAudioFocusRequest(it)
        }
        audioFocusRequest = null
    }

    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(CHANNEL_ID, "AS2P", NotificationManager.IMPORTANCE_LOW)
            getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
        }
    }

    private fun sendGoodbyeToServer() {
        val target = serverAddress ?: return
        // Use a raw Thread to ensure it outlives the Service context
        Thread {
            try {
                val socket = DatagramSocket()
                val data = "AS2P_GOODBYE".toByteArray()
                val packet = DatagramPacket(data, data.size, target, 12345)
                socket.send(packet)
                socket.close()
                Log.i("AS2P_Service", "Goodbye Sent to Server (Thread)")
            } catch (e: Exception) {
                e.printStackTrace()
            }
        }.start()
    }

    override fun onDestroy() {
        isRunning = false
        // stopSelf() only schedules the teardown: without this the coroutines
        // launched by this instance keep running after onDestroy returns, and the
        // receiver loop can still be parked in receive() holding UDP 12345 when
        // the next instance tries to bind it (EADDRINUSE).
        serviceScope.cancel()
        sendGoodbyeToServer() // 主動告訴 Server 我要斷開了
        NativeBridge.setResendCallback(null)
        NativeBridge.stopNative()
        udpSocket?.close()
        resendSocket?.close()
        resendSocket = null
        resendTarget = null
        releasePowerLocks()
        abandonAudioFocus()
        _serviceState.value = false
        _isSearchingFlow.value = false
        _latencyFlow.value = 0.0f
        _latencyHistoryFlow.value = emptyList()
        mediaSession?.release()
        
        try { unregisterReceiver(bluetoothReceiver) } catch (e: Exception) {}
        super.onDestroy()
    }
        override fun onBind(intent: Intent?) = null
    
        override fun onTaskRemoved(rootIntent: Intent?) {
            log("Task Removed. Stopping Service...")
            stopSelf()
            super.onTaskRemoved(rootIntent)
        }
    }
    