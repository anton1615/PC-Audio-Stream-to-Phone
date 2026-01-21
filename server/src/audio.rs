use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use rubato::{SincFixedIn, SincInterpolationType, SincInterpolationParameters, WindowFunction};
use colored::*;

pub enum CaptureEvent {
    Data(Vec<f32>),
    DeviceChanged(String),
    Error(String),
}

pub struct Fader {
    current_volume: f32,
    target_volume: f32,
    step: f32, // Change per sample
}

impl Fader {
    pub fn new() -> Self {
        Self {
            current_volume: 1.0,
            target_volume: 1.0,
            step: 0.0,
        }
    }

    pub fn fade_to(&mut self, target: f32, duration_ms: f32, sample_rate: f32) {
        self.target_volume = target;
        let total_samples = (duration_ms / 1000.0) * sample_rate;
        if total_samples > 0.0 {
            self.step = (target - self.current_volume) / total_samples;
        } else {
            self.current_volume = target;
            self.step = 0.0;
        }
    }

    pub fn process(&mut self, samples: &mut [f32]) {
        if self.step == 0.0 && self.current_volume == self.target_volume {
            if self.current_volume != 1.0 {
                for s in samples.iter_mut() { *s *= self.current_volume; }
            }
            return;
        }

        for s in samples.iter_mut() {
            *s *= self.current_volume;
            if (self.step > 0.0 && self.current_volume < self.target_volume) ||
               (self.step < 0.0 && self.current_volume > self.target_volume) {
                self.current_volume += self.step;
            } else {
                self.current_volume = self.target_volume;
                self.step = 0.0;
            }
        }
    }

    pub fn is_faded_out(&self) -> bool {
        self.current_volume <= 0.001 && self.target_volume <= 0.0
    }
}

pub struct AudioCapturer {
    _stream: Option<cpal::Stream>,
    receiver: Receiver<Vec<f32>>,
    sender: Sender<Vec<f32>>,
    pub current_device_name: String,
    stream_failed: Arc<AtomicBool>,
    pub needs_restart: Arc<AtomicBool>,
    last_callback_time: Instant,
    fader: Fader,
    debug_enabled: bool,
}

impl AudioCapturer {
    pub fn new(debug: bool) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        let mut capturer = Self {
            _stream: None,
            receiver: rx,
            sender: tx,
            current_device_name: String::new(),
            stream_failed: Arc::new(AtomicBool::new(false)),
            needs_restart: Arc::new(AtomicBool::new(false)),
            last_callback_time: Instant::now(),
            fader: Fader::new(),
            debug_enabled: debug,
        };
        capturer.reinitialize()?;
        
        let needs_restart_clone = capturer.needs_restart.clone();
        let initial_name = capturer.current_device_name.clone();
        std::thread::spawn(move || {
            let mut last_name = initial_name;
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let host = cpal::default_host();
                if let Some(device) = host.default_output_device() {
                    if let Ok(name) = device.name() {
                        if name != last_name {
                            last_name = name;
                            needs_restart_clone.store(true, Ordering::SeqCst);
                        }
                    }
                }
            }
        });
        Ok(capturer)
    }

    pub fn start_fade_out(&mut self) {
        if self.debug_enabled { println!("{}", "[AUDIO] Initiating Fade-out (10ms)".yellow()); }
        self.fader.fade_to(0.0, 10.0, 48000.0);
    }

    pub fn is_fade_complete(&self) -> bool {
        self.fader.is_faded_out()
    }

    pub fn reinitialize(&mut self) -> Result<(), String> {
        self.stream_failed.store(false, Ordering::SeqCst);
        self.needs_restart.store(false, Ordering::SeqCst);
        self.last_callback_time = Instant::now();
        self.fader.current_volume = 0.0; // Start at 0 for fade-in
        self.fader.fade_to(1.0, 10.0, 48000.0);
        
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("No output device")?;
        #[allow(deprecated)]
        let device_name = device.name().unwrap_or_else(|_| "Unknown".to_string());
        
        // For WASAPI Loopback, we use the OUTPUT config of the device
        let config = device.default_output_config().map_err(|e| e.to_string())?;
        let input_sample_rate = config.sample_rate() as f64;
        let channels = config.channels() as usize;

        if self.debug_enabled {
            println!("{} {} ({}Hz, {}ch)", "[AUDIO] Capture Device:".green(), device_name.cyan(), input_sample_rate, channels);
        }

        let stream_config: cpal::StreamConfig = config.clone().into();
        let tx = self.sender.clone();
        let failed = self.stream_failed.clone();

        // Setup Resampler if needed
        let mut resampler = if input_sample_rate != 48000.0 || channels != 2 {
            let params = SincInterpolationParameters {
                sinc_len: 256,
                f_cutoff: 0.95,
                interpolation: SincInterpolationType::Linear,
                oversampling_factor: 128,
                window: WindowFunction::BlackmanHarris2,
            };
            Some(SincFixedIn::<f32>::new(
                48000.0 / input_sample_rate,
                2.0,
                params,
                1920, // chunk size
                channels,
            ).map_err(|e| e.to_string())?)
        } else {
            None
        };

        let sample_format = config.sample_format();
        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                device.build_input_stream(
                    &stream_config,
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        let pcm = data.to_vec();
                        if let Some(ref mut _rs) = resampler {
                            // TODO: Implement actual resampling logic if needed
                            let _ = tx.send(pcm);
                        } else {
                            let _ = tx.send(pcm);
                        }
                    },
                    move |_| { failed.store(true, Ordering::SeqCst); },
                    None,
                )
            }
            // Add other formats similarly...
            _ => {
                 device.build_input_stream(
                    &stream_config,
                    move |data: &[f32], _: &cpal::InputCallbackInfo| { let _ = tx.send(data.to_vec()); },
                    move |_| { failed.store(true, Ordering::SeqCst); },
                    None,
                )
            }
        }.map_err(|e| e.to_string())?;

        stream.play().map_err(|e| e.to_string())?;
        self._stream = Some(stream);
        self.current_device_name = device_name;
        Ok(())
    }

    pub fn next_event(&mut self) -> CaptureEvent {
        if self.needs_restart.load(Ordering::SeqCst) || self.stream_failed.load(Ordering::SeqCst) {
            match self.reinitialize() {
                Ok(_) => return CaptureEvent::DeviceChanged(self.current_device_name.clone()),
                Err(e) => return CaptureEvent::Error(e),
            }
        }
        
        match self.receiver.try_recv() {
            Ok(mut data) => {
                self.last_callback_time = Instant::now();
                self.fader.process(&mut data);
                CaptureEvent::Data(data)
            }
            _ => {
                if self.last_callback_time.elapsed().as_millis() >= 25 {
                    self.last_callback_time = Instant::now();
                    let mut silence = vec![0.0f32; 1920];
                    self.fader.process(&mut silence);
                    CaptureEvent::Data(silence)
                } else {
                    CaptureEvent::Data(vec![])
                }
            }
        }
    }
}
