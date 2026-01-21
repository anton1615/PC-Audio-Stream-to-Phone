use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::mpsc::{self, Receiver};
use std::time::{Instant, Duration};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct AudioCapturer {
    _stream: Option<cpal::Stream>,
    receiver: Receiver<Vec<f32>>,
    sender: mpsc::Sender<Vec<f32>>,
    device_name: String,
    last_check: Instant,
    stream_failed: Arc<AtomicBool>,
}

impl AudioCapturer {
    pub fn new() -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        let mut capturer = Self {
            _stream: None,
            receiver: rx,
            sender: tx,
            device_name: String::new(),
            last_check: Instant::now(),
            stream_failed: Arc::new(AtomicBool::new(false)),
        };
        
        capturer.reinitialize()?;
        Ok(capturer)
    }

    fn reinitialize(&mut self) -> Result<(), String> {
        self.stream_failed.store(false, Ordering::SeqCst);
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("No default output device found")?;

        #[allow(deprecated)]
        let device_name = device.name().unwrap_or_else(|_| "Unknown Device".to_string());
        self.device_name = device_name.clone();
        
        let supported_configs: Vec<_> = device.supported_output_configs()
            .map_err(|e| e.to_string())?
            .filter(|c| {
                let min_hz: u32 = c.min_sample_rate().into(); 
                let max_hz: u32 = c.max_sample_rate().into();
                c.channels() == 2 && 48000 >= min_hz && 48000 <= max_hz
            })
            .collect();

        if supported_configs.is_empty() {
            return Err(format!("Device {} does not support 48kHz stereo.", device_name));
        }

        let config = supported_configs.iter()
            .find(|c| c.sample_format() == cpal::SampleFormat::F32)
            .or_else(|| supported_configs.iter().find(|c| c.sample_format() == cpal::SampleFormat::I16))
            .or_else(|| supported_configs.first())
            .unwrap()
            .clone()
            .with_sample_rate(48000);

        let sample_format = config.sample_format();
        println!("Hot-reloading audio capture: [{}] using {:?}", device_name, sample_format);
        
        let stream_config: cpal::StreamConfig = config.into();
        let tx = self.sender.clone();
        let failed = self.stream_failed.clone();
        
        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                device.build_input_stream(
                    &stream_config,
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        let _ = tx.send(data.to_vec());
                    },
                    move |err| {
                        eprintln!("Stream error: {}", err);
                        failed.store(true, Ordering::SeqCst);
                    },
                    None,
                )
            }
            cpal::SampleFormat::I16 => {
                device.build_input_stream(
                    &stream_config,
                    move |data: &[i16], _: &cpal::InputCallbackInfo| {
                        let f32_data: Vec<f32> = data.iter().map(|&s| s as f32 / 32768.0).collect();
                        let _ = tx.send(f32_data);
                    },
                    move |err| {
                        eprintln!("Stream error: {}", err);
                        failed.store(true, Ordering::SeqCst);
                    },
                    None,
                )
            }
            cpal::SampleFormat::U16 => {
                device.build_input_stream(
                    &stream_config,
                    move |data: &[u16], _: &cpal::InputCallbackInfo| {
                        let f32_data: Vec<f32> = data.iter().map(|&s| (s as f32 - 32768.0) / 32768.0).collect();
                        let _ = tx.send(f32_data);
                    },
                    move |err| {
                        eprintln!("Stream error: {}", err);
                        failed.store(true, Ordering::SeqCst);
                    },
                    None,
                )
            }
            cpal::SampleFormat::U8 => {
                device.build_input_stream(
                    &stream_config,
                    move |data: &[u8], _: &cpal::InputCallbackInfo| {
                        let f32_data: Vec<f32> = data.iter().map(|&s| (s as f32 - 128.0) / 128.0).collect();
                        let _ = tx.send(f32_data);
                    },
                    move |err| {
                        eprintln!("Stream error: {}", err);
                        failed.store(true, Ordering::SeqCst);
                    },
                    None,
                )
            }
            _ => return Err(format!("Unsupported sample format: {:?}", sample_format)),
        }.map_err(|e| e.to_string())?;

        stream.play().map_err(|e| e.to_string())?;
        self._stream = Some(stream);
        Ok(())
    }

    pub fn read_samples(&mut self) -> Result<Option<Vec<f32>>, String> {
        // 每 2 秒檢查一次裝置一致性或流狀態
        if self.last_check.elapsed() > Duration::from_secs(2) || self.stream_failed.load(Ordering::SeqCst) {
            self.last_check = Instant::now();
            
            let device_changed = !self.check_device_consistency();
            let has_failed = self.stream_failed.load(Ordering::SeqCst);

            if device_changed || has_failed {
                if device_changed {
                    println!("Detected audio device change. Reinitializing...");
                } else {
                    println!("Detected stream failure. Attempting recovery...");
                }
                
                // 嘗試重啟，如果失敗則繼續下一次嘗試
                if let Err(e) = self.reinitialize() {
                    eprintln!("Failed to reinitialize audio capturer: {}", e);
                }
            }
        }

        match self.receiver.try_recv() {
            Ok(data) => Ok(Some(data)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err("Audio channel disconnected".to_string()),
        }
    }

    fn check_device_consistency(&self) -> bool {
        let host = cpal::default_host();
        match host.default_output_device() {
            Some(device) => {
                #[allow(deprecated)]
                match device.name() {
                    Ok(name) => name == self.device_name,
                    Err(_) => false,
                }
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_capturer_initialization() {
        let capturer = AudioCapturer::new();
        if let Ok(c) = capturer {
            assert!(!c.device_name.is_empty());
        }
    }
}