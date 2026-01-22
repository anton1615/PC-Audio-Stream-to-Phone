use audiopus::{coder::Encoder, Application, Channels, Bitrate, Signal, Error, SampleRate};

pub fn create_encoder() -> Result<Encoder, Error> {
    // 改為 Application::Audio 以獲得最佳音質
    let mut encoder = Encoder::new(SampleRate::Hz48000, Channels::Stereo, Application::Audio)?;
    encoder.set_bitrate(Bitrate::Auto)?;
    encoder.set_vbr(false)?; // 關閉 VBR，改用 CBR 以減少安靜時的量化雜訊
    // 注意：audiopus 0.2.0 未直接暴露 set_dtx，故移除。
    // 在 Application::Audio 模式下，DTX 預設通常是關閉的。
    Ok(encoder)
}

pub fn configure_encoder(encoder: &mut Encoder, bitrate: i32, complexity: i32) -> Result<(), Error> {
    let (valid_bitrate, valid_complexity) = validate_config(bitrate, complexity);
    // 使用正確的 BitsPerSecond 變體
    let br = if valid_bitrate <= 0 { Bitrate::Auto } else { Bitrate::BitsPerSecond(valid_bitrate) };
    encoder.set_bitrate(br)?;
    encoder.set_complexity(valid_complexity.clamp(0, 10) as u8)?;
    encoder.set_signal(Signal::Music)?;
    Ok(())
}

/// 驗證並限制音訊配置參數。
/// Bitrate 限制在 16,000bps (16kbps) 到 512,000bps (512kbps) 之間。
/// Complexity 限制在 0 到 10 之間。
pub fn validate_config(bitrate: i32, complexity: i32) -> (i32, i32) {
    let valid_bitrate = bitrate.clamp(16_000, 512_000);
    let valid_complexity = complexity.clamp(0, 10);
    (valid_bitrate, valid_complexity)
}

pub fn encode_frame(encoder: &mut Encoder, pcm: &[f32]) -> Vec<u8> {
    // 確保數據在 -1.0 到 1.0 之間，防止數位溢出雜訊
    let mut clipped_pcm = pcm.to_vec();
    for sample in clipped_pcm.iter_mut() {
        if *sample > 1.0 { *sample = 1.0; }
        if *sample < -1.0 { *sample = -1.0; }
    }

    let mut out_buffer = vec![0u8; 4000];
        match encoder.encode_float(&clipped_pcm, &mut out_buffer) {
            Ok(size) => {
                out_buffer.truncate(size);
                out_buffer
            }
            Err(e) => {
                eprintln!("Encode error: {:?}", e);
                Vec::new()
            }
        }
    }
    
    #[cfg(test)]
    mod tests {
        use super::*;
    
        #[test]
        fn test_validate_config_normal() {
            let (br, comp) = validate_config(160_000, 5);
            assert_eq!(br, 160_000);
            assert_eq!(comp, 5);
        }
    
        #[test]
        fn test_validate_config_too_low() {
            let (br, comp) = validate_config(500, -1);
            assert_eq!(br, 16_000);
            assert_eq!(comp, 0);
        }
    
        #[test]
        fn test_validate_config_too_high() {
            let (br, comp) = validate_config(1_000_000, 15);
            assert_eq!(br, 512_000);
            assert_eq!(comp, 10);
        }
    }
    