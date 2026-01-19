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
    // 使用正確的 BitsPerSecond 變體
    let br = if bitrate <= 0 { Bitrate::Auto } else { Bitrate::BitsPerSecond(bitrate) };
    encoder.set_bitrate(br)?;
    encoder.set_complexity(complexity.clamp(0, 10) as u8)?;
    encoder.set_signal(Signal::Music)?;
    Ok(())
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