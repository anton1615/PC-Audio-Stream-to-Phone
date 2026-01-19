use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::mpsc::{self, Receiver};

pub struct AudioCapturer {
    _stream: cpal::Stream,
    receiver: Receiver<Vec<f32>>,
    pub device_name: String,
}

impl AudioCapturer {
    pub fn new() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or("No default output device found")?;

        let device_name = device.name().unwrap_or_else(|_| "Unknown Device".to_string());
        
        // 獲取所有支援 48kHz Stereo 的配置
        let supported_configs: Vec<_> = device.supported_output_configs()
            .map_err(|e| e.to_string())?
            .filter(|c| {
                let min_hz: u32 = c.min_sample_rate().into(); 
                let max_hz: u32 = c.max_sample_rate().into();
                c.channels() == 2 && 48000 >= min_hz && 48000 <= max_hz
            })
            .collect();

        if supported_configs.is_empty() {
            return Err(format!("Device {} does not support 48kHz stereo. Please check Windows sound settings.", device_name));
        }

        // 優先順序：F32 > I16 > 其他
        let config = supported_configs.iter()
            .find(|c| c.sample_format() == cpal::SampleFormat::F32)
            .or_else(|| supported_configs.iter().find(|c| c.sample_format() == cpal::SampleFormat::I16))
            .or_else(|| supported_configs.first())
            .unwrap()
            .clone()
            .with_sample_rate(48000);

        let sample_format = config.sample_format();
        println!("Capturing audio from [{}] using {:?} at 48000Hz Stereo", device_name, sample_format);
        
        let stream_config: cpal::StreamConfig = config.into();
        let (tx, rx) = mpsc::channel();
        
        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                device.build_input_stream(
                    &stream_config,
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        let _ = tx.send(data.to_vec());
                    },
                    |err| eprintln!("Stream error: {}", err),
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
                    |err| eprintln!("Stream error: {}", err),
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
                    |err| eprintln!("Stream error: {}", err),
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
                    |err| eprintln!("Stream error: {}", err),
                    None,
                )
            }
            _ => return Err(format!("Unsupported sample format: {:?}", sample_format)),
        }.map_err(|e| e.to_string())?;

        stream.play().map_err(|e| e.to_string())?;

        Ok(Self {
            _stream: stream,
            receiver: rx,
            device_name,
        })
    }

    pub fn read_samples(&mut self) -> Result<Option<Vec<f32>>, String> {
        match self.receiver.try_recv() {
            Ok(data) => Ok(Some(data)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err("Audio channel disconnected".to_string()),
        }
    }

    pub fn get_current_default_device_name() -> String {
        cpal::default_host()
            .default_output_device()
            .and_then(|d| d.name().ok())
            .unwrap_or_else(|| "None".to_string())
    }
}
