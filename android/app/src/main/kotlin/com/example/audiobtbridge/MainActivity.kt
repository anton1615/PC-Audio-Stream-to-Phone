package com.example.audiobtbridge

import android.content.Context
import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.animation.*
import androidx.compose.animation.core.*
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.example.audiobtbridge.latency.LatencyMode

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        
        // 1. Load Persistence
        val prefs = getSharedPreferences("AS2P_Prefs", Context.MODE_PRIVATE)
        val savedModeName = prefs.getString("latency_mode", LatencyMode.BALANCE.name)
        val initialMode = try { LatencyMode.valueOf(savedModeName!!) } catch (e: Exception) { LatencyMode.BALANCE }
        AudioService.setModeOffline(initialMode)

        val initialPlc = prefs.getBoolean("plc_enabled", true)
        AudioService.updatePlcEnabled(initialPlc)

        setContent {
            MaterialTheme(colorScheme = darkColorScheme(
                primary = Color(0xFF64FFDA),
                surface = Color(0xFF121212),
                background = Color(0xFF0A0A0A),
                error = Color(0xFFFF5252)
            )) {
                Surface(color = MaterialTheme.colorScheme.background) {
                    MainScreen(this)
                }
            }
        }
    }
}

@Composable
fun LatencyChart(history: List<Float>, modifier: Modifier = Modifier) {
    val primaryColor = MaterialTheme.colorScheme.primary
    Box(modifier = modifier.height(120.dp).fillMaxWidth().background(Color.Black.copy(0.3f), RoundedCornerShape(16.dp)).padding(8.dp)) {
        Canvas(modifier = Modifier.fillMaxSize()) {
            if (history.size < 2) return@Canvas
            val maxLatency = 200f // 固定刻度以便觀察
            val width = size.width
            val height = size.height
            val stepX = width / 60f
            val path = Path()
            history.takeLast(60).forEachIndexed { index, value ->
                val x = index * stepX
                val y = height - (value.coerceIn(0f, maxLatency) / maxLatency * height)
                if (index == 0) path.moveTo(x, y) else path.lineTo(x, y)
            }
            drawPath(path = path, color = primaryColor, style = Stroke(width = 2.dp.toPx()))
        }
    }
}

@Composable
fun MainScreen(context: Context) {
    val serviceState by AudioService.serviceState.collectAsState()
    val latency by AudioService.latencyFlow.collectAsState()
    val history by AudioService.latencyHistoryFlow.collectAsState()
    val currentMode by AudioService.latencyModeFlow.collectAsState()
    val isSearching by AudioService.isSearchingFlow.collectAsState()

    Column(modifier = Modifier.fillMaxSize().padding(20.dp), horizontalAlignment = Alignment.CenterHorizontally) {
        Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween, verticalAlignment = Alignment.CenterVertically) {
            Text("PC Audio Stream to Phone", style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.Black, color = MaterialTheme.colorScheme.primary)
        }
        
        if (isSearching && serviceState) {
            Spacer(modifier = Modifier.height(8.dp))
            LinearProgressIndicator(modifier = Modifier.fillMaxWidth().height(2.dp), color = MaterialTheme.colorScheme.primary)
            Text("Searching for Server...", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.primary)
        }

        Spacer(modifier = Modifier.height(20.dp))

        // Latency Info Card
        Card(modifier = Modifier.fillMaxWidth(), shape = RoundedCornerShape(24.dp)) {
            Column(modifier = Modifier.padding(20.dp), horizontalAlignment = Alignment.CenterHorizontally) {
                Text(text = if (serviceState) "ACTIVE" else "IDLE", color = if (serviceState) Color.Green else Color.Gray)
                Text(text = "%.1f".format(latency), style = MaterialTheme.typography.displayMedium, fontWeight = FontWeight.Bold)
                Text(text = "LATENCY (MS)", style = MaterialTheme.typography.labelSmall)
                Spacer(modifier = Modifier.height(16.dp))
                LatencyChart(history = history)
            }
        }

        Spacer(modifier = Modifier.height(20.dp))

        // Preset Selection
        val radioOptions = listOf(LatencyMode.LOW_LATENCY, LatencyMode.BALANCE, LatencyMode.HIGH_QUALITY, LatencyMode.BEST_QUALITY)
        Column(modifier = Modifier.selectableGroup().fillMaxWidth()) {
            radioOptions.forEach { mode ->
                val selected = mode == currentMode
                Surface(
                    modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp).selectable(
                        selected = selected,
                        onClick = {
                            // Save to Prefs
                            val prefs = context.getSharedPreferences("AS2P_Prefs", Context.MODE_PRIVATE)
                            prefs.edit().putString("latency_mode", mode.name).apply()

                            AudioService.setModeOffline(mode)
                            val intent = Intent(context, AudioService::class.java).apply {
                                action = AudioService.ACTION_UPDATE_MODE
                                putExtra(AudioService.EXTRA_MODE, mode.name)
                            }
                            context.startService(intent)
                        },
                        role = Role.RadioButton
                    ),
                    shape = RoundedCornerShape(12.dp),
                    color = if (selected) MaterialTheme.colorScheme.primary.copy(0.1f) else Color.Transparent
                ) {
                    Row(modifier = Modifier.padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
                        RadioButton(selected = selected, onClick = null)
                        Text(text = mode.name.replace("_", " "), modifier = Modifier.padding(start = 16.dp))
                    }
                }
            }
        }

        Spacer(modifier = Modifier.height(20.dp))

        // PLC Toggle
        var plcEnabled by remember { 
            mutableStateOf(context.getSharedPreferences("AS2P_Prefs", Context.MODE_PRIVATE).getBoolean("plc_enabled", true)) 
        }
        
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.SpaceBetween
        ) {
            Column {
                Text("Packet Loss Concealment", fontWeight = FontWeight.Bold)
                Text("Reduce robotic artifacts by disabling", style = MaterialTheme.typography.labelSmall, color = Color.Gray)
            }
            Switch(
                checked = plcEnabled,
                onCheckedChange = { enabled ->
                    plcEnabled = enabled
                    context.getSharedPreferences("AS2P_Prefs", Context.MODE_PRIVATE)
                        .edit().putBoolean("plc_enabled", enabled).apply()
                    AudioService.updatePlcEnabled(enabled)
                }
            )
        }

        Spacer(modifier = Modifier.weight(1f))

        Button(
            onClick = {
                val intent = Intent(context, AudioService::class.java).apply {
                    action = if (serviceState) AudioService.ACTION_STOP else AudioService.ACTION_START
                }
                if (serviceState) context.startService(intent) else context.startForegroundService(intent)
            },
            modifier = Modifier.fillMaxWidth().height(64.dp),
            shape = RoundedCornerShape(32.dp),
            colors = ButtonDefaults.buttonColors(containerColor = if (serviceState) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.primary)
        ) {
            Text(if (serviceState) "STOP" else "CONNECT", fontWeight = FontWeight.Bold)
        }
    }
}
