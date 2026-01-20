package com.example.audiobtbridge

import android.app.*
import android.content.*
import android.media.AudioManager
import android.net.wifi.WifiManager
import android.os.*
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
import com.example.audiobtbridge.latency.AudioConfig
import com.example.audiobtbridge.latency.LatencyMode

import android.support.v4.media.session.MediaSessionCompat
import android.support.v4.media.session.PlaybackStateCompat

class AudioService : Service() {

    private val serviceJob = Job()
    private val serviceScope = CoroutineScope(Dispatchers.IO + serviceJob)
    private var udpSocket: DatagramSocket? = null
    private var isRunning = false
    private var serverAddress: InetAddress? = null
    private var lastPacketTime: Long = 0
    private var bluetoothReceiver: BroadcastReceiver? = null
    private var mediaSession: MediaSessionCompat? = null
    
    private var wakeLock: PowerManager.WakeLock? = null
    private var wifiLock: WifiManager.WifiLock? = null
    private var audioDeviceCallback: Any? = null

    private val connectionManager = ConnectionManager()
    private val latencyManager = LatencyManager()

    companion object {
        private val _serviceState = MutableStateFlow(false)
        val serviceState = _serviceState.asStateFlow()

        private var packetCount = 0
        private val _packetCountFlow = MutableStateFlow(0)
        val packetCountFlow = _packetCountFlow.asStateFlow()

        private val _latencyModeFlow = MutableStateFlow(LatencyMode.BALANCE)
        val latencyModeFlow = _latencyModeFlow.asStateFlow()
        
        fun setModeOffline(mode: LatencyMode) {
            _latencyModeFlow.value = mode
        }
        
        private val _clientHelloCount = MutableStateFlow(0)
        val clientHelloCount = _clientHelloCount.asStateFlow()

        private val _lastSequenceFlow = MutableStateFlow(0L)
        val lastSequenceFlow = _lastSequenceFlow.asStateFlow()

        private val _latencyFlow = MutableStateFlow(0.0f)
        val latencyFlow = _latencyFlow.asStateFlow()

        const val CHANNEL_ID = "AudioServiceChannel"
        const val NOTIFICATION_ID = 1
        const val ACTION_STOP = "STOP_SERVICE"
        const val ACTION_UPDATE_MODE = "UPDATE_MODE"
        const val EXTRA_MODE = "MODE"

        @JvmStatic
        fun updateStats(seq: Long, latency: Float) {
            _lastSequenceFlow.value = seq
            _latencyFlow.value = latency
        }
    }

    override fun onCreate() {
        super.onCreate()
        registerBluetoothReceiver()
        registerAudioDeviceCallback()
    }

    private fun registerAudioDeviceCallback() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
            val audioManager = getSystemService(Context.AUDIO_SERVICE) as AudioManager
            val callback = object : android.media.AudioDeviceCallback() {
                override fun onAudioDevicesAdded(addedDevices: Array<out android.media.AudioDeviceInfo>?) {
                    Log.i("AudioBT", "Audio device added, checking for BT...")
                    addedDevices?.forEach {
                        if (it.type == android.media.AudioDeviceInfo.TYPE_BLUETOOTH_A2DP ||
                            it.type == android.media.AudioDeviceInfo.TYPE_BLUETOOTH_SCO) {
                            Log.i("AudioBT", "Bluetooth device detected, restarting native stream...")
                            serviceScope.launch {
                                delay(1000) // 等待系統完成切換
                                NativeBridge.stopNative()
                                delay(200)
                                NativeBridge.initNative()
                                val currentConfig = latencyManager.getConfigForMode(_latencyModeFlow.value)
                                NativeBridge.setBufferSize(currentConfig.bufferSize)
                            }
                        }
                    }
                }
            }
            audioManager.registerAudioDeviceCallback(callback, Handler(Looper.getMainLooper()))
            audioDeviceCallback = callback
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                stopSelf()
                return START_NOT_STICKY
            }
            ACTION_UPDATE_MODE -> {
                val modeName = intent.getStringExtra(EXTRA_MODE)
                if (modeName != null) {
                    try {
                        val newMode = LatencyMode.valueOf(modeName)
                        updateLatencyMode(newMode)
                    } catch (e: Exception) { }
                }
            }
        }

        if (!isRunning) {
            try {
                NativeBridge.initNative()
            } catch (e: Throwable) {
                Log.e("AudioBT", "Failed to initialize native bridge: ${e.message}")
            }
            
            val prefs = getSharedPreferences("prefs", MODE_PRIVATE)
            val savedMode = prefs.getString("preset", "BALANCE")?.uppercase() ?: "BALANCE"
            val initialMode = try { LatencyMode.valueOf(savedMode) } catch(e: Exception) { LatencyMode.BALANCE }
            _latencyModeFlow.value = initialMode

            acquireLocks()
            startForegroundService()
            startAudioStream()
        }

        return START_STICKY
    }

    private fun acquireLocks() {
        val powerManager = getSystemService(Context.POWER_SERVICE) as PowerManager
        wakeLock = powerManager.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "AudioBT:WakeLock").apply {
            acquire()
        }

        val wifiManager = getSystemService(Context.WIFI_SERVICE) as WifiManager
        val wifiMode = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            WifiManager.WIFI_MODE_FULL_LOW_LATENCY
        } else {
            WifiManager.WIFI_MODE_FULL_HIGH_PERF
        }
        wifiLock = wifiManager.createWifiLock(wifiMode, "AudioBT:WifiLock").apply {
            acquire()
        }
        Log.i("AudioBT", "Locks acquired: WakeLock and WifiLock (Low Latency Mode)")
    }

    private fun releaseLocks() {
        wakeLock?.let {
            if (it.isHeld) it.release()
        }
        wakeLock = null

        wifiLock?.let {
            if (it.isHeld) it.release()
        }
        wifiLock = null
        Log.i("AudioBT", "Locks released")
    }

    private fun startForegroundService() {
        createNotificationChannel()
        
        // Setup MediaSession for background priority
        mediaSession = MediaSessionCompat(this, "AudioBTBridge").apply {
            setPlaybackState(PlaybackStateCompat.Builder()
                .setState(PlaybackStateCompat.STATE_PLAYING, 0, 1.0f)
                .build())
            isActive = true
        }
        
        val stopIntent = Intent(this, AudioService::class.java).apply { action = ACTION_STOP }
        val stopPendingIntent = PendingIntent.getService(this, 0, stopIntent, PendingIntent.FLAG_IMMUTABLE)

        val notification = NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle("PC Audio Steam to Phone")
            .setContentText("Receiving audio...")
            .setSmallIcon(android.R.drawable.ic_media_play)
            .setOngoing(true)
            .setStyle(androidx.media.app.NotificationCompat.MediaStyle()
                .setMediaSession(mediaSession?.sessionToken))
            .addAction(android.R.drawable.ic_menu_close_clear_cancel, "Disconnect", stopPendingIntent)
            .build()

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            val type = if (Build.VERSION.SDK_INT >= 34) {
                android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK
            } else {
                android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE
            }
            startForeground(NOTIFICATION_ID, notification, type)
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
        isRunning = true
        _serviceState.value = true
    }

    private fun startAudioStream() {
        // 統計數據輪詢任務
        serviceScope.launch {
            while (isRunning) {
                val seq = NativeBridge.getLastSequence()
                val depth = NativeBridge.getBufferDepth()
                // 每個 Opus 包通常是 20ms
                val latencyMs = depth * 20.0f
                
                _lastSequenceFlow.value = seq
                _latencyFlow.value = latencyMs
                delay(100) // 每 100ms 更新一次 UI
            }
        }

        serviceScope.launch {
            // 設置為高優先級，減少背景爆裂聲
            android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_URGENT_AUDIO)
            try {
                val socket = DatagramSocket(12345)
                socket.receiveBufferSize = 1024 * 1024
                udpSocket = socket
                
                val buffer = ByteArray(2048)
                val packet = DatagramPacket(buffer, buffer.size)

                val initialConfig = latencyManager.getConfigForMode(_latencyModeFlow.value)
                NativeBridge.setBufferSize(initialConfig.bufferSize)

                while (isRunning) {
                    socket.receive(packet)
                    val isHello = packet.length == 12 && packet.data[0] == 'A'.code.toByte()
                    val isDisconnect = packet.length == 1 && packet.data[0] == 0x03.toByte()

                    if (isDisconnect) {
                        Log.i("AudioBT", "Received Disconnect command from Server")
                        stopSelf()
                        break
                    } else if (!isHello) {
                        if (serverAddress == null) {
                            serverAddress = packet.address
                            sendConfigToServer(latencyManager.getConfigForMode(_latencyModeFlow.value))
                        }
                        
                        packetCount++
                        _packetCountFlow.value = packetCount
                        NativeBridge.writeToNativeBuffer(packet.data, packet.length)
                        lastPacketTime = System.currentTimeMillis()
                    }
                }
            } catch (e: Exception) {
                if (isRunning) Log.e("AudioBT", "Receiver error: ${e.message}")
            } finally {
                udpSocket?.close()
            }
        }

        serviceScope.launch {
            val socket = DatagramSocket()
            socket.broadcast = true
            val msg = "AS2P_HELLO__".toByteArray()
            val packet = DatagramPacket(msg, msg.size, InetAddress.getByName("255.255.255.255"), 12345)
            
            while (isRunning) {
                if (connectionManager.shouldSendHello(System.currentTimeMillis(), lastPacketTime)) {
                     try {
                        socket.send(packet)
                        _clientHelloCount.value += 1
                    } catch (e: Exception) { }
                }
                delay(1000)
            }
            socket.close()
        }
    }

    private fun updateLatencyMode(mode: LatencyMode) {
        Log.i("AudioBT", "Switching to mode: ${mode.name}")
        _latencyModeFlow.value = mode
        val config = latencyManager.getConfigForMode(mode)
        NativeBridge.setBufferSize(config.bufferSize)
        sendConfigToServer(config)
    }

    private fun sendConfigToServer(config: AudioConfig) {
        val target = serverAddress ?: return
        // 使用 Dispatchers.Main.immediate 或直接啟動以減少切換延遲
        serviceScope.launch(Dispatchers.IO) {
            try {
                val socket = DatagramSocket()
                val buffer = ByteBuffer.allocate(6).order(ByteOrder.LITTLE_ENDIAN)
                buffer.put(0x02.toByte())
                buffer.putInt(config.bitrate)
                buffer.put(config.complexity.toByte())
                val data = buffer.array()
                val packet = DatagramPacket(data, data.size, target, 12345)
                socket.send(packet)
                socket.close()
                Log.d("AudioBT", "Config sent to server: ${config.bitrate}bps")
            } catch (e: Exception) {
                Log.e("AudioBT", "Failed to send config: ${e.message}")
            }
        }
    }

    private fun sendDisconnectToServer() {
        val target = serverAddress ?: return
        Thread {
            try {
                val socket = DatagramSocket()
                val packet = DatagramPacket(byteArrayOf(0x03), 1, target, 12345)
                socket.send(packet)
                socket.close()
            } catch (e: Exception) { }
        }.start()
    }

    override fun onDestroy() {
        isRunning = false
        sendDisconnectToServer()
        
        releaseLocks()
        _serviceState.value = false
        packetCount = 0
        _packetCountFlow.value = 0
        _clientHelloCount.value = 0
        _lastSequenceFlow.value = 0L
        _latencyFlow.value = 0.0f
        serverAddress = null
        
        udpSocket?.close()
        NativeBridge.stopNative()
        serviceJob.cancel()
        bluetoothReceiver?.let { unregisterReceiver(it) }
        super.onDestroy()
    }

    private fun registerBluetoothReceiver() {
        val filter = IntentFilter().apply {
            addAction(android.bluetooth.BluetoothDevice.ACTION_ACL_DISCONNECTED)
            addAction(android.bluetooth.BluetoothDevice.ACTION_ACL_CONNECTED)
            addAction(AudioManager.ACTION_AUDIO_BECOMING_NOISY)
        }
        bluetoothReceiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context?, intent: Intent?) {
                when (intent?.action) {
                    android.bluetooth.BluetoothDevice.ACTION_ACL_DISCONNECTED -> {
                        Log.i("AudioBT", "Bluetooth ACL disconnected, stopping service")
                        stopSelf()
                    }
                    android.bluetooth.BluetoothDevice.ACTION_ACL_CONNECTED -> {
                        Log.i("AudioBT", "Bluetooth ACL connected, re-initializing stream")
                        serviceScope.launch {
                            delay(500)
                            NativeBridge.stopNative()
                            delay(300)
                            NativeBridge.initNative()
                            val currentConfig = latencyManager.getConfigForMode(_latencyModeFlow.value)
                            NativeBridge.setBufferSize(currentConfig.bufferSize)
                        }
                    }
                    AudioManager.ACTION_AUDIO_BECOMING_NOISY -> {
                        Log.i("AudioBT", "Audio becoming noisy (Headset unplugged), stopping service")
                        stopSelf()
                    }
                }
            }
        }
        registerReceiver(bluetoothReceiver, filter)
    }

    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val serviceChannel = NotificationChannel(CHANNEL_ID, "Audio Service Channel", NotificationManager.IMPORTANCE_LOW)
            val manager = getSystemService(NotificationManager::class.java)
            manager.createNotificationChannel(serviceChannel)
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null
}
