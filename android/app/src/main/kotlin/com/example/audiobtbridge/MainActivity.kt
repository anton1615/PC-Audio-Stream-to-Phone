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
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.RoundedCornerShape
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
        setContent {
            // Force Dark Theme for modern tech look
            MaterialTheme(
                colorScheme = darkColorScheme(
                    primary = Color(0xFF00E5FF), // Cyan
                    secondary = Color(0xFF7C4DFF), // Deep Purple
                    background = Color(0xFF0A0A0A), // Near Black
                    surface = Color(0xFF161616), // Dark Gray
                    onSurface = Color.White,
                    error = Color(0xFFFF5252)
                )
            ) {
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
fun LatencyChart(history: List<Float>, modifier: Modifier = Modifier) {
    val primaryColor = MaterialTheme.colorScheme.primary
    
    Box(modifier = modifier.height(120.dp).fillMaxWidth()) {
        Canvas(modifier = Modifier.fillMaxSize()) {
            if (history.isEmpty()) return@Canvas
            
            val maxLatency = history.maxOrNull()?.coerceAtLeast(100f) ?: 100f
            val width = size.width
            val height = size.height
            val stepX = width / 60f
            
            val path = Path()
            history.forEachIndexed { index, value ->
                val x = index * stepX
                val y = height - (value / maxLatency * height)
                if (index == 0) path.moveTo(x, y) else path.lineTo(x, y)
            }
            
            drawPath(
                path = path,
                color = primaryColor,
                style = Stroke(width = 2.dp.toPx())
            )
        }
    }
}

@Composable
fun MainScreen(context: Context) {
    val serviceState by AudioService.serviceState.collectAsState()
    val latency by AudioService.latencyFlow.collectAsState()
    val history by AudioService.latencyHistoryFlow.collectAsState()
    val currentMode by AudioService.latencyModeFlow.collectAsState()

    val prefs = context.getSharedPreferences("prefs", Context.MODE_PRIVATE)

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(20.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(20.dp)
    ) {
        Spacer(modifier = Modifier.height(10.dp))
        
        Text(
            text = "AS2P",
            style = MaterialTheme.typography.displaySmall,
            fontWeight = FontWeight.Black,
            color = MaterialTheme.colorScheme.primary
        )

        // Status Card
        Card(
            modifier = Modifier.fillMaxWidth(),
            shape = RoundedCornerShape(24.dp),
            colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surface)
        ) {
            Column(
                modifier = Modifier.padding(24.dp),
                horizontalAlignment = Alignment.CenterHorizontally
            ) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.Center
                ) {
                    Surface(
                        modifier = Modifier.size(12.dp),
                        shape = RoundedCornerShape(6.dp),
                        color = if (serviceState) Color.Green else Color.Red
                    ) {}
                    Spacer(modifier = Modifier.width(8.dp))
                    Text(
                        text = if (serviceState) "ACTIVE STREAM" else "IDLE",
                        style = MaterialTheme.typography.labelLarge,
                        color = if (serviceState) Color.Green else Color.Red,
                        letterSpacing = 2.sp
                    )
                }
                
                Spacer(modifier = Modifier.height(16.dp))
                
                Text(
                    text = "%.1f".format(latency),
                    style = MaterialTheme.typography.displayLarge,
                    fontWeight = FontWeight.Bold
                )
                Text(
                    text = "BUFFER LATENCY (MS)",
                    style = MaterialTheme.typography.labelSmall,
                    color = Color.Gray
                )
                
                Spacer(modifier = Modifier.height(24.dp))
                
                LatencyChart(history = history)
            }
        }

        // Preset Selection
        Text(
            text = "AUDIO PRESETS",
            style = MaterialTheme.typography.labelLarge,
            color = Color.Gray,
            modifier = Modifier.align(Alignment.Start).padding(start = 8.dp)
        )
        
        Card(
            modifier = Modifier.fillMaxWidth(),
            shape = RoundedCornerShape(24.dp),
            colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surface)
        ) {
            val radioOptions = listOf(
                LatencyMode.LOW_LATENCY, 
                LatencyMode.BALANCE, 
                LatencyMode.HIGH_QUALITY,
                LatencyMode.BEST_QUALITY
            )
            Column(
                modifier = Modifier.selectableGroup().padding(vertical = 8.dp)
            ) {
                radioOptions.forEach { mode ->
                    val selected = mode == currentMode
                    Row(
                        Modifier
                            .fillMaxWidth()
                            .height(56.dp)
                            .selectable(
                                selected = selected,
                                onClick = {
                                    prefs.edit().putString("preset", mode.name).apply()
                                    AudioService.setModeOffline(mode)
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
                            .padding(horizontal = 24.dp),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        RadioButton(
                            selected = selected,
                            onClick = null,
                            colors = RadioButtonDefaults.colors(selectedColor = MaterialTheme.colorScheme.primary)
                        )
                        Text(
                            text = mode.name.replace("_", " "),
                            style = MaterialTheme.typography.bodyLarge,
                            fontWeight = if (selected) FontWeight.Bold else FontWeight.Normal,
                            modifier = Modifier.padding(start = 16.dp),
                            color = if (selected) MaterialTheme.colorScheme.primary else Color.White
                        )
                    }
                }
            }
        }

        Spacer(modifier = Modifier.weight(1f))

        // Toggle Button
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
            modifier = Modifier.fillMaxWidth().height(64.dp),
            shape = RoundedCornerShape(32.dp),
            colors = if (serviceState) 
                ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error) 
                else ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.secondary)
        ) {
            Text(
                text = if (serviceState) "STOP STREAMING" else "START STREAMING",
                style = MaterialTheme.typography.titleMedium,
                fontWeight = FontWeight.Bold
            )
        }
        
        Spacer(modifier = Modifier.height(10.dp))
    }
}