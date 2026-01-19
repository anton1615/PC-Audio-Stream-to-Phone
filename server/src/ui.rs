use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use eframe::egui;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tokio::sync::mpsc as tokio_mpsc;

#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
#[cfg(target_os = "windows")]
use windows::Win32::Foundation::HWND;

pub enum UiMessage {
    UpdateStats {
        packets: u64,
        bitrate: i32,
        client_ip: Option<String>,
    },
}

#[derive(Clone, Copy)]
pub enum UiCommand {
    ToggleServer,
    ClientDisconnect, // 新增：手機遠端斷開
    Quit,
}

pub struct AudioServerApp {
    pub is_running: Arc<AtomicBool>,
    pub is_visible: Arc<AtomicBool>,
    pub bitrate: i32,
    pub packets_sent: u64,
    pub client_ip: Option<String>,
    pub receiver: std::sync::mpsc::Receiver<UiMessage>,
    pub cmd_sender: tokio_mpsc::Sender<UiCommand>,
    pub hwnd_store: Arc<Mutex<Option<isize>>>,
}

impl AudioServerApp {
    pub fn new(
        _cc: &eframe::CreationContext<'_>, 
        receiver: std::sync::mpsc::Receiver<UiMessage>,
        cmd_sender: tokio_mpsc::Sender<UiCommand>,
        is_running: Arc<AtomicBool>,
        is_visible: Arc<AtomicBool>,
        hwnd_store: Arc<Mutex<Option<isize>>>,
    ) -> Self {
        Self {
            is_running,
            is_visible,
            bitrate: 128000,
            packets_sent: 0,
            client_ip: None,
            receiver,
            cmd_sender,
            hwnd_store,
        }
    }
}

impl eframe::App for AudioServerApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // 獲取 HWND
        let hwnd_val = {
            let mut store = self.hwnd_store.lock().unwrap();
            if store.is_none() {
                if let Ok(handle) = frame.window_handle() {
                    if let RawWindowHandle::Win32(h) = handle.as_raw() {
                        *store = Some(h.hwnd.get() as isize);
                    }
                }
            }
            *store
        };

        // 處理視窗關閉事件
        if ctx.input(|i| i.viewport().close_requested()) {
            println!("[UI] Window close requested. Sending Quit command.");
            let _ = self.cmd_sender.try_send(UiCommand::Quit);
        }

        while let Ok(msg) = self.receiver.try_recv() {
            match msg {
                UiMessage::UpdateStats { packets, bitrate, client_ip } => {
                    self.packets_sent = packets;
                    self.bitrate = bitrate;
                    self.client_ip = client_ip;
                }
            }
        }

        if !self.is_visible.load(Ordering::SeqCst) {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
            return;
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("AS2P Server (PC Audio Steam to Phone)");
            
            let running = self.is_running.load(Ordering::SeqCst);
            ui.horizontal(|ui| {
                ui.label("Status:");
                if running {
                    ui.colored_label(egui::Color32::GREEN, "Running");
                } else {
                    ui.colored_label(egui::Color32::RED, "Stopped");
                }
            });

            if ui.button(if running { "Stop Server" } else { "Start Server" }).clicked() {
                let _ = self.cmd_sender.try_send(UiCommand::ToggleServer);
            }

            ui.separator();
            ui.label(format!("Bitrate: {} bps", self.bitrate));
            ui.label(format!("Packets: {}", self.packets_sent));
            if let Some(ip) = &self.client_ip {
                ui.label(format!("Client: {}", ip));
            }

            ui.add_space(20.0);
            if ui.button("Minimize to Tray").clicked() {
                self.is_visible.store(false, Ordering::SeqCst);
                #[cfg(target_os = "windows")]
                if let Some(h) = hwnd_val {
                    unsafe { let _ = ShowWindow(HWND(h as _), SW_HIDE); }
                }
            }
        });
        
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}
