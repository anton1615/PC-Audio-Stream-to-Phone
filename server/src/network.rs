use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use colored::*;
use rand::Rng;

/// Audio packet header, version 9.
///
/// Layout (all integers little-endian):
/// ```text
///   0..10   "AS2P_AUDIO"
///  10..18   media_tick    u64  monotonic frame counter (20 ms per tick)
///  18..26   send_ts_ms    u64  sender wall clock at transmit time
///  26..34   flags         u64  see FLAG_* below
///  34..38   seq           u32  transmit order, used for gap detection
///  38..     opus payload
/// ```
///
/// `media_tick` and `send_ts_ms` are deliberately separate. The receiver must
/// decide "is this packet late?" from the media timeline (tick), never from the
/// arrival time — a retransmitted packet is old by arrival time but still exactly
/// on time in the media timeline. The previous v8 header carried only a sequence
/// and a timestamp, which conflated the two and made correct jitter accounting
/// impossible.
pub const AUDIO_PREFIX: &[u8] = b"AS2P_AUDIO";
pub const AUDIO_HEADER_LEN: usize = 38;

/// Packet carries a payload the sender had to resend on request.
pub const FLAG_RETRANSMIT: u64 = 1 << 0;

/// Duration of one media tick in milliseconds. Opus frames are 20 ms.
pub const TICK_MS: u64 = 20;

/// Client asks the server to resend a specific tick: `AS2P_RESEND` + tick(u64).
pub const RESEND_PREFIX: &[u8] = b"AS2P_RESEND";
pub const RESEND_LEN: usize = RESEND_PREFIX.len() + 8;

/// How many recently sent packets to keep for retransmission.
const RESEND_CACHE_SIZE: usize = 128;

pub struct CachedPacket {
    pub tick: u64,
    pub send_ts_ms: u64,
    pub payload: Vec<u8>,
}

/// Holds recently sent packets so the receiver can ask for a resend instead of
/// the sender blindly transmitting everything twice.
///
/// The previous approach (`redundancy_enabled`) spent 100% extra bandwidth on
/// every packet whether or not anything was lost. Requesting only the packets
/// that actually went missing costs nothing in the common case.
pub struct ResendCache {
    entries: std::collections::VecDeque<CachedPacket>,
}

impl ResendCache {
    pub fn new() -> Self {
        Self {
            entries: std::collections::VecDeque::with_capacity(RESEND_CACHE_SIZE),
        }
    }

    pub fn store(&mut self, tick: u64, send_ts_ms: u64, payload: &[u8]) {
        if self.entries.len() >= RESEND_CACHE_SIZE {
            self.entries.pop_front();
        }
        self.entries.push_back(CachedPacket {
            tick,
            send_ts_ms,
            payload: payload.to_vec(),
        });
    }

    pub fn get(&self, tick: u64) -> Option<&CachedPacket> {
        self.entries.iter().find(|e| e.tick == tick)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

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

    /// Builds a v9 audio packet. Kept separate from sending so the retransmit
    /// path can rebuild the exact same packet with FLAG_RETRANSMIT set.
    pub fn build_audio_packet(
        seq: u32,
        tick: u64,
        send_ts_ms: u64,
        flags: u64,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut packet = Vec::with_capacity(AUDIO_HEADER_LEN + payload.len());
        packet.extend_from_slice(AUDIO_PREFIX);
        packet.extend_from_slice(&tick.to_le_bytes());
        packet.extend_from_slice(&send_ts_ms.to_le_bytes());
        packet.extend_from_slice(&flags.to_le_bytes());
        packet.extend_from_slice(&seq.to_le_bytes());
        packet.extend_from_slice(payload);
        packet
    }

    pub async fn send_audio(
        &mut self,
        seq: u32,
        tick: u64,
        send_ts_ms: u64,
        payload: Vec<u8>,
    ) -> Result<(), String> {
        // Drop simulation
        if self.drop_rate > 0.0 {
            let mut rng = rand::thread_rng();
            if rng.r#gen::<f32>() < self.drop_rate {
                return Ok(());
            }
        }

        let packet = Self::build_audio_packet(seq, tick, send_ts_ms, 0, &payload);
        self.socket.send_to(&packet, self.target).await.map_err(|e| e.to_string())?;

        // Legacy blind redundancy. Superseded by the resend cache, but kept so
        // the UI toggle keeps working for users on lossy links who would rather
        // pay the bandwidth than wait for a round trip.
        if self.redundancy_enabled {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            self.socket.send_to(&packet, self.target).await.map_err(|e| e.to_string())?;
        }

        Ok(())
    }

    /// Resends a cached packet, marked so the receiver can tell it apart from
    /// fresh audio and account for it correctly in its late-packet statistics.
    pub async fn send_retransmission(
        &mut self,
        seq: u32,
        cached: &CachedPacket,
    ) -> Result<(), String> {
        let packet = Self::build_audio_packet(
            seq,
            cached.tick,
            cached.send_ts_ms,
            FLAG_RETRANSMIT,
            &cached.payload,
        );
        if self.debug_enabled {
            println!(
                "{} Resending tick {} (seq {})",
                "[NET]".bright_black(),
                cached.tick,
                seq
            );
        }
        self.socket.send_to(&packet, self.target).await.map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Parses an `AS2P_RESEND` request. Returns the requested media tick.
pub fn parse_resend_request(buf: &[u8]) -> Option<u64> {
    if buf.len() < RESEND_LEN || &buf[0..11] != RESEND_PREFIX {
        return None;
    }
    let mut tick_bytes = [0u8; 8];
    tick_bytes.copy_from_slice(&buf[11..19]);
    Some(u64::from_le_bytes(tick_bytes))
}

pub fn build_resend_request(tick: u64) -> Vec<u8> {
    let mut packet = Vec::with_capacity(RESEND_LEN);
    packet.extend_from_slice(RESEND_PREFIX);
    packet.extend_from_slice(&tick.to_le_bytes());
    packet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_packet_layout_is_stable() {
        let payload = [0xAAu8, 0xBB, 0xCC];
        let packet = UdpSender::build_audio_packet(7, 1234, 5678, FLAG_RETRANSMIT, &payload);

        assert_eq!(&packet[0..10], AUDIO_PREFIX);
        assert_eq!(packet.len(), AUDIO_HEADER_LEN + payload.len());
        assert_eq!(u64::from_le_bytes(packet[10..18].try_into().unwrap()), 1234);
        assert_eq!(u64::from_le_bytes(packet[18..26].try_into().unwrap()), 5678);
        assert_eq!(u64::from_le_bytes(packet[26..34].try_into().unwrap()), FLAG_RETRANSMIT);
        assert_eq!(u32::from_le_bytes(packet[34..38].try_into().unwrap()), 7);
        assert_eq!(&packet[38..], &payload);
    }

    #[test]
    fn resend_request_round_trips() {
        let request = build_resend_request(4242);
        assert_eq!(request.len(), RESEND_LEN);
        assert_eq!(&request[0..11], RESEND_PREFIX);
        assert_eq!(parse_resend_request(&request), Some(4242));
    }

    #[test]
    fn resend_request_rejects_short_or_foreign_packets() {
        assert_eq!(parse_resend_request(b"AS2P_RESEND"), None);
        assert_eq!(parse_resend_request(b"AS2P_AUDIOxxxxxxxxxxxxxxxxxxxx"), None);
    }

    #[test]
    fn resend_cache_evicts_oldest_and_finds_by_tick() {
        let mut cache = ResendCache::new();
        for tick in 0..(RESEND_CACHE_SIZE as u64 + 10) {
            cache.store(tick, tick * TICK_MS, &[tick as u8]);
        }

        // The oldest entries were evicted, the newest are still present.
        assert!(cache.get(0).is_none());
        assert_eq!(cache.get(RESEND_CACHE_SIZE as u64 + 9).map(|e| e.tick), Some(RESEND_CACHE_SIZE as u64 + 9));
    }
}
