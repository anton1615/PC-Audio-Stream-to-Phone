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

/// MMCSS (Multimedia Class Scheduler Service) registration for the audio thread.
///
/// SetPriorityClass(HIGH_PRIORITY_CLASS) only raises the *process* base priority;
/// it does not give the audio thread the guaranteed scheduling slots MMCSS
/// reserves for registered "Pro Audio" threads. Without this, glitch-free
/// capture depends on nothing else on the box competing for the CPU.
///
/// `AvSetMmThreadCharacteristicsW` must be called from the thread it applies to,
/// so this is invoked at the top of the CPAL callback's owning thread. The
/// returned handle has to stay alive for the registration to persist and must be
/// reverted from the same thread, hence the RAII guard.
#[cfg(windows)]
pub struct MmcssGuard {
    handle: windows::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl MmcssGuard {
    /// Registers the calling thread with MMCSS under the "Pro Audio" task.
    /// Returns `None` when the registration fails (e.g. ERROR_PRIVILEGE_NOT_HELD
    /// 1314), in which case the caller keeps running at its normal priority.
    pub fn register() -> Option<Self> {
        use windows::core::w;
        use windows::Win32::System::Threading::AvSetMmThreadCharacteristicsW;

        let mut task_index: u32 = 0;
        // SAFETY: `w!("Pro Audio")` is a valid NUL-terminated wide string and
        // `task_index` is a valid out-parameter for the duration of the call.
        // The returned HANDLE is owned by this guard and reverted in `drop`.
        let handle = unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) }.ok()?;
        Some(Self { handle })
    }
}

#[cfg(windows)]
impl Drop for MmcssGuard {
    fn drop(&mut self) {
        use windows::Win32::System::Threading::AvRevertMmThreadCharacteristics;
        // SAFETY: `self.handle` came from a successful AvSetMmThreadCharacteristicsW
        // call on this same thread and is reverted exactly once.
        unsafe {
            let _ = AvRevertMmThreadCharacteristics(self.handle);
        }
    }
}

#[cfg(not(windows))]
pub struct MmcssGuard;

#[cfg(not(windows))]
impl MmcssGuard {
    pub fn register() -> Option<Self> {
        None
    }
}

/// Registers the *current* thread with MMCSS exactly once.
///
/// CPAL invokes the capture callback on a dedicated audio thread, but the
/// callback itself is the only code that runs there, so the registration has to
/// happen on the first invocation. `THREAD_LOCAL` holds the guard for the
/// lifetime of that thread, which is what keeps the MMCSS registration alive —
/// reverting it immediately would defeat the purpose.
///
/// `AvSetMmThreadCharacteristicsW` only needs to be called once per thread; the
/// `Cell<bool>` short-circuits the repeated calls that would otherwise happen on
/// every audio buffer (~every 10 ms).
fn register_audio_thread_once(debug_enabled: bool) {
    thread_local! {
        static MMCSS: std::cell::RefCell<Option<MmcssGuard>> = const { std::cell::RefCell::new(None) };
        static REGISTERED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    REGISTERED.with(|registered| {
        if registered.get() {
            return;
        }
        registered.set(true);

        let guard = MmcssGuard::register();
        if debug_enabled {
            match guard {
                Some(_) => println!("{} Audio thread registered with MMCSS (Pro Audio)", "[SYS]".green()),
                None => println!("{} MMCSS registration failed; audio thread runs at normal priority", "[SYS]".yellow()),
            }
        }
        MMCSS.with(|slot| *slot.borrow_mut() = guard);
    });
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
    silence_credit_us: u128,
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
            silence_credit_us: 0,
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
        self.silence_credit_us = 0;
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
        let debug_enabled = self.debug_enabled;
        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                device.build_input_stream(
                    &stream_config,
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        register_audio_thread_once(debug_enabled);
                        let pcm = data.to_vec();
                        if let Some(_rs) = &mut resampler {
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
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        register_audio_thread_once(debug_enabled);
                        let _ = tx.send(data.to_vec());
                    },
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
                self.silence_credit_us = 0;
                self.fader.process(&mut data);
                CaptureEvent::Data(data)
            }
            _ => {
                // Silence fallback: WASAPI loopback fires only while something
                // renders, so a fully silent PC delivers no callbacks at all.
                //
                // Emit silence at real time — one 20 ms frame per 20 ms of
                // wall clock. The old flat one-frame-per-25 ms gate measured
                // ~32 frames/s against the client's 50 frames/s consumption;
                // the deficit drained the jitter buffer during every silent
                // period and the underrun storm ratcheted the adaptive
                // watermark to its ceiling, so audio that resumed afterwards
                // paid up to 500 ms of added latency.
                let elapsed = self.last_callback_time.elapsed();
                if elapsed < Duration::from_millis(40) {
                    // 40 ms gate stays safely above the ~10-20 ms real
                    // callback cadence so the fallback cannot interleave
                    // false silence into live audio.
                    CaptureEvent::Data(vec![])
                } else if elapsed > Duration::from_millis(250) {
                    // Long stall (device reinit): give up the backlog.
                    self.last_callback_time = Instant::now();
                    self.silence_credit_us = 0;
                    let mut silence = vec![0.0f32; 1920];
                    self.fader.process(&mut silence);
                    CaptureEvent::Data(silence)
                } else {
                    // Convert the elapsed wall time into whole 20 ms frames,
                    // keeping the sub-frame remainder in a microsecond credit
                    // so the long-run rate stays locked to real time no
                    // matter how the poll loop's ticks land.
                    self.silence_credit_us += elapsed.as_micros();
                    let n = (self.silence_credit_us / 20_000) as usize;
                    self.silence_credit_us -= n as u128 * 20_000;
                    self.last_callback_time = Instant::now();
                    let mut silence = vec![0.0f32; 1920 * n];
                    self.fader.process(&mut silence);
                    CaptureEvent::Data(silence)
                }
            }
        }
    }
}
