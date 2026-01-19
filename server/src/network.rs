use std::net::SocketAddr;
use tokio::net::UdpSocket;

pub struct UdpSender {
    socket: UdpSocket,
    target: SocketAddr,
}

impl UdpSender {
    pub async fn new(target: &str) -> Result<Self, String> {
        let socket = UdpSocket::bind("0.0.0.0:0").await.map_err(|e| e.to_string())?;
        let target_addr: SocketAddr = target.parse().map_err(|e: std::net::AddrParseError| e.to_string())?;
        Ok(Self {
            socket,
            target: target_addr,
        })
    }

    pub async fn send_audio_with_seq(&mut self, sequence: u64, payload: Vec<u8>) -> Result<(), String> {
        let mut packet = Vec::with_capacity(8 + payload.len());
        packet.extend_from_slice(&sequence.to_le_bytes());
        packet.extend_from_slice(&payload);
        
        self.socket.send_to(&packet, self.target).await.map_err(|e| e.to_string())?;
        Ok(())
    }

    pub async fn send_raw(&mut self, data: &[u8]) -> Result<(), String> {
        self.socket.send_to(data, self.target).await.map_err(|e| e.to_string())?;
        Ok(())
    }
}
