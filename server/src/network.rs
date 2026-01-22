use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use colored::*;
use rand::Rng;

pub struct UdpSender {
    socket: Arc<UdpSocket>,
    target: SocketAddr,
    pub debug_enabled: bool,
    pub redundancy_enabled: bool,
    pub drop_rate: f32,
}

impl UdpSender {
    pub fn new(socket: Arc<UdpSocket>, target: SocketAddr, debug: bool, redundancy: bool, drop_rate: f32) -> Self {
        Self {
            socket,
            target,
            debug_enabled: debug,
            redundancy_enabled: redundancy,
            drop_rate,
        }
    }

    pub async fn send_control(&mut self, prefix: &str) -> Result<(), String> {
        if self.debug_enabled {
            println!("{} Sent: {}", "[CONN]".blue(), prefix.bold());
        }
        self.socket.send_to(prefix.as_bytes(), self.target).await.map_err(|e: std::io::Error| e.to_string())?;
        Ok(())
    }

    pub async fn send_audio_v8(&mut self, sequence: u64, timestamp: u64, payload: Vec<u8>) -> Result<(), String> {
        // Drop simulation
        if self.drop_rate > 0.0 {
            let mut rng = rand::thread_rng();
            if rng.r#gen::<f32>() < self.drop_rate {
                return Ok(());
            }
        }

        // AS2P_AUDIO Prefix + Seq(8) + TS(8) + Data
        let prefix = b"AS2P_AUDIO";
        let mut packet = Vec::with_capacity(prefix.len() + 8 + 8 + payload.len());
        packet.extend_from_slice(prefix);
        packet.extend_from_slice(&sequence.to_le_bytes());
        packet.extend_from_slice(&timestamp.to_le_bytes());
        packet.extend_from_slice(&payload);

        self.socket.send_to(&packet, self.target).await.map_err(|e: std::io::Error| e.to_string())?;
        
        // Redundancy: Send twice if enabled, with a small delay to improve time diversity
        if self.redundancy_enabled {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            self.socket.send_to(&packet, self.target).await.map_err(|e: std::io::Error| e.to_string())?;
        }
        
        Ok(())
    }
}

