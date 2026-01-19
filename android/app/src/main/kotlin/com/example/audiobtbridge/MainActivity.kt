package com.example.audiobtbridge

import android.content.Context
import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import com.example.audiobtbridge.latency.LatencyMode

class MainActivity : ComponentActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        
        setContent {
            MaterialTheme {
                Surface(
                    modifier = Modifier.fillMaxSize(),
                    color = MaterialTheme.colorScheme.background
                ) {
                    MainScreen(this)
                }
            }
        }
    }
}

@Composable
fun MainScreen(context: Context) {
    val serviceState by AudioService.serviceState.collectAsState()
    val packetCount by AudioService.packetCountFlow.collectAsState()
    val lastSequence by AudioService.lastSequenceFlow.collectAsState()
    val latency by AudioService.latencyFlow.collectAsState()
    val handshakeCount by AudioService.clientHelloCount.collectAsState()
    val currentMode by AudioService.latencyModeFlow.collectAsState()

    val prefs = context.getSharedPreferences("prefs", Context.MODE_PRIVATE)

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(16.dp)
    ) {
        Text(
            text = "AudioBT-Bridge",
            style = MaterialTheme.typography.headlineMedium
        )

        Card(
            modifier = Modifier.fillMaxWidth()
        ) {
            Column(
                modifier = Modifier.padding(16.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp)
            ) {
                Text("Status: ${if (serviceState) "Connected" else "Disconnected"}")
                Text("Packets Received: $packetCount")
                Text("Last Sequence: $lastSequence")
                Text("Buffer Latency: %.1f ms".format(latency))
                Text("Handshake Count: $handshakeCount")
            }
        }

        Spacer(modifier = Modifier.height(8.dp))

        Text("Preset Selection:", style = MaterialTheme.typography.titleMedium)
        
        val radioOptions = listOf(LatencyMode.STABLE, LatencyMode.BALANCED, LatencyMode.LOW_LATENCY)
        Column(Modifier.selectableGroup()) {
            radioOptions.forEach { mode ->
                Row(
                    Modifier
                        .fillMaxWidth()
                        .height(48.dp)
                        .selectable(
                            selected = (mode == currentMode),
                            onClick = {
                                prefs.edit().putString("preset", mode.name).apply()
                                AudioService.setModeOffline(mode) // 更新靜態 Flow 讓 UI 即時響應
                                if (serviceState) {
                                    val intent = Intent(context, AudioService::class.java).apply {
                                        action = AudioService.ACTION_UPDATE_MODE
                                        putExtra(AudioService.EXTRA_MODE, mode.name)
                                    }
                                    context.startService(intent)
                                }
                            },
                            role = Role.RadioButton
                        )
                        .padding(horizontal = 16.dp),
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    RadioButton(
                        selected = (mode == currentMode),
                        onClick = null // null recommended for accessibility with selectable modifier
                    )
                    Text(
                        text = mode.name,
                        style = MaterialTheme.typography.bodyLarge,
                        modifier = Modifier.padding(start = 16.dp)
                    )
                }
            }
        }

        Spacer(modifier = Modifier.weight(1f))

        Button(
            onClick = {
                if (serviceState) {
                    val intent = Intent(context, AudioService::class.java).apply { action = AudioService.ACTION_STOP }
                    context.startService(intent)
                } else {
                    val intent = Intent(context, AudioService::class.java)
                    if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.O) {
                        context.startForegroundService(intent)
                    } else {
                        context.startService(intent)
                    }
                }
            },
            modifier = Modifier.fillMaxWidth(),
            colors = if (serviceState) ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error) 
                     else ButtonDefaults.buttonColors()
        ) {
            Text(if (serviceState) "Disconnect" else "Connect")
        }
    }
}
