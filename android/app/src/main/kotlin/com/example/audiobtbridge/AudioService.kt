package com.example.audiobtbridge

import android.app.*
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.bluetooth.BluetoothDevice
import android.media.AudioDeviceCallback
import android.media.AudioDeviceInfo
import android.media.AudioManager
import android.os.*
import android.support.v4.media.session.MediaSessionCompat
import android.support.v4.media.session.PlaybackStateCompat
import android.util.Log
import androidx.core.app.NotificationCompat
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetAddress
import java.nio.ByteBuffer
import java.nio.ByteOrder
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
        const val EXTRA_MODE = "EXTRA_MODE"
        private const val CHANNEL_ID = "audio_stream_channel"

        fun setModeOffline(mode: LatencyMode) { _latencyModeFlow.value = mode }
        
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
    }

    private val bluetoothReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            val action = intent?.action
            if (BluetoothDevice.ACTION_ACL_DISCONNECTED == action || AudioManager.ACTION_AUDIO_BECOMING_NOISY == action) {
                if (serverAddress != null) {
                    log("Audio Device Disconnected. Terminating Connection...")
                    // 1. Send Goodbye immediately
                    sendGoodbyeToServer()
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
                }
            }
            override fun onAudioDevicesRemoved(removedDevices: Array<out AudioDeviceInfo>?) {
                if (isRunning) {
                    log("Audio Device Removed: Resetting Buffer...")
                    NativeBridge.resetAudio()
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
            ACTION_START -> if (!isRunning) startAudioService()
            ACTION_STOP -> {
                // Do not clear serverAddress here, as it's needed by sendGoodbyeToServer() in onDestroy()
                stopSelf()
            }
            ACTION_UPDATE_MODE -> {
                val modeName = intent.getStringExtra(EXTRA_MODE)
                modeName?.let { updateLatencyMode(LatencyMode.valueOf(it)) }
            }
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
        
        isRunning = true
        _serviceState.value = true
        _isSearchingFlow.value = true
        NativeBridge.initNative()
        
        // --- Sync initial preset to Native layer ---
        val initialConfig = latencyManager.getConfigForMode(_latencyModeFlow.value)
        NativeBridge.setBufferSize(initialConfig.bufferSize)
        Log.i("AS2P_Diag", "[Init] Synced initial Buffer Size: ${initialConfig.bufferSize}")

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
                                                        sendConfigToServer(latencyManager.getConfigForMode(_latencyModeFlow.value))
                                                        lastAudioTime = currentTime // 給 Server 一點反應時間
                                                    } else {
                                                        log("Connection lost (Timeout). Back to searching...")
                                                        mediaSession?.setPlaybackState(PlaybackStateCompat.Builder().setState(PlaybackStateCompat.STATE_PAUSED, 0, 1.0f).build())
                                                        serverAddress = null
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
                val latency = NativeBridge.getBufferDepth() * 20.0f
                _latencyFlow.value = latency
                val history = _latencyHistoryFlow.value.toMutableList()
                history.add(latency)
                if (history.size > 60) history.removeAt(0)
                _latencyHistoryFlow.value = history
                delay(100)
            }
        }

        // Receiver Loop
                var duplicateCount = 0
                var lastLogTime = 0L
                var lastReceivedSeq = -1L

        serviceScope.launch(Dispatchers.IO) {
            try {
                val socket = DatagramSocket(12345)
                socket.receiveBufferSize = 1024 * 1024
                udpSocket = socket
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
                                Log.i("AS2P_Diag", "[Conn] Server Address set to: ${packet.address}")
                                _isSearchingFlow.value = false
                                mediaSession?.setPlaybackState(PlaybackStateCompat.Builder().setState(PlaybackStateCompat.STATE_PLAYING, 0, 1.0f).build())
                                log("Server Found: ${packet.address}")
                                NativeBridge.resetAudio()
                                connectionManager.reset()
                                lastReceivedSeq = -1L // Reset tracker
                                sendConfigToServer(latencyManager.getConfigForMode(_latencyModeFlow.value))
                                lastAudioTime = System.currentTimeMillis()
                                configRetryCount = 0
                            }
                        } else if (len > 12 && String(data, 0, 10) == "AS2P_AUDIO") {
                            _isSearchingFlow.value = false 
                            lastAudioTime = System.currentTimeMillis()
                            configRetryCount = 0
                            
                            if (len > 26) { // Prefix(10) + Seq(8) + TS(8)
                                // Redundancy Check
                                val seq = ByteBuffer.wrap(data, 10, 8).order(ByteOrder.LITTLE_ENDIAN).long
                                
                                if (lastReceivedSeq != -1L && seq > lastReceivedSeq + 1) {
                                    Log.w("AS2P_Diag", "[Network] Gap: ${lastReceivedSeq + 1} to ${seq - 1}")
                                }
                                lastReceivedSeq = seq

                                if (connectionManager.isDuplicate(seq)) {
                                    duplicateCount++
                                }

                                val now = System.currentTimeMillis()
                                if (now - lastLogTime > 2000) {
                                    val plcCount = NativeBridge.getPLCCount()
                                    if (duplicateCount > 0 || plcCount > 0) {
                                        // Log to Logcat only, do not update notification
                                        Log.i("AS2P_Service", "[Stats] Redundancy: $duplicateCount, PLC: $plcCount (in 2s)")
                                        
                                        // Refresh notification to show "Streaming" state cleanly
                                        val modeName = _latencyModeFlow.value.name.replace("_", " ")
                                        updateNotificationState("$modeName - Streaming")
                                    }
                                    duplicateCount = 0
                                    lastLogTime = now
                                }

                                val payloadSize = len - 10
                                val payload = ByteArray(payloadSize)
                                System.arraycopy(data, 10, payload, 0, payloadSize)
                                NativeBridge.writeToNativeBuffer(payload, payloadSize)
                            }
                        } else if (len >= 12 && String(data, 0, 12) == "AS2P_GOODBYE") {
                            log("Server Stopped. Resetting...")
                            mediaSession?.setPlaybackState(PlaybackStateCompat.Builder().setState(PlaybackStateCompat.STATE_PAUSED, 0, 1.0f).build())
                            serverAddress = null
                            _isSearchingFlow.value = true
                            NativeBridge.resetAudio()
                            connectionManager.reset()
                        }
                    } catch (e: java.net.SocketTimeoutException) {
                        if (serverAddress != null) {
                            log("Connection Timeout. Searching...")
                            serverAddress = null
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
            } catch (e: Exception) { 
                if (isRunning) log("Socket Error: ${e.message}") 
            } finally { udpSocket?.close() }
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
            val socket = DatagramSocket()
            socket.broadcast = true
            val msg = "AS2P_DISCOVER".toByteArray()
            val packet = DatagramPacket(msg, msg.size, InetAddress.getByName("255.255.255.255"), 12345)
            log("Discovery Loop Started (Interval: 1000ms)")
            
            while (isRunning) {
                if (serverAddress == null) {
                    try { 
                        socket.send(packet)
                        // log("Sent Discovery...") // Too spammy
                    } catch (e: Exception) {}
                }
                delay(1000)
            }
            socket.close()
        }
    }

    private fun updateLatencyMode(mode: LatencyMode) {
        _latencyModeFlow.value = mode
        val config = latencyManager.getConfigForMode(mode)
        NativeBridge.setBufferSize(config.bufferSize)
        sendConfigToServer(config)
        log("Mode Updated: ${mode.name}")
    }

    private fun sendConfigToServer(config: AudioConfig) {
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
        sendGoodbyeToServer() // 主動告訴 Server 我要斷開了
        NativeBridge.stopNative()
        udpSocket?.close()
        _serviceState.value = false
        _isSearchingFlow.value = false
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
    