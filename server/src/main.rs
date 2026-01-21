// #![windows_subsystem = "windows"]

use std::sync::{Arc};
use tokio::net::UdpSocket;
use tokio::sync::mpsc as tokio_mpsc;
use std::sync::atomic::{AtomicBool, Ordering};
use tray_icon::{
    menu::{Menu, MenuItem, MenuEvent},
    TrayIconBuilder, TrayIconEvent, Icon
};
use socket2::{Socket, Domain, Type, Protocol, SockAddr};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, TranslateMessage, DispatchMessageW, MSG,
    FindWindowW, ShowWindow, SW_HIDE, SW_SHOW, SW_RESTORE, SetForegroundWindow, IsWindowVisible,
    PostQuitMessage
};
use windows::Win32::Graphics::Gdi::{InvalidateRect, UpdateWindow};
use windows::Win32::Foundation::HWND;
use windows::core::{w};
use crossbeam_channel::{unbounded};
use slint::ComponentHandle;
use image::ImageReader;
use std::io::Cursor;
use clap::Parser;
use colored::*;

mod audio;
mod encoder;
mod network;
mod ui;

use crate::audio::{AudioCapturer, CaptureEvent};
use crate::encoder::{create_encoder, encode_frame, configure_encoder};
use crate::network::UdpSender;
use crate::ui::{UiMessage, UiCommand, UiState, AppWindow};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value_t = false)]
    debug: bool,
    #[arg(long, default_value_t = 0.0)]
    drop_rate: f32,
}

#[derive(PartialEq, Debug)]
enum ServerState {
    Idle,
    Listening,
    Streaming,
    Stopping,
}

fn load_embedded_icon() -> Icon {
    let icon_data = include_bytes!("../../as2p.png");
    let img = ImageReader::new(Cursor::new(icon_data)).with_guessed_format().expect("Err").decode().expect("Err").into_rgba8();
    let width = img.width(); let height = img.height();
    Icon::from_rgba(img.into_raw(), width, height).expect("Err")
}

fn force_window_redraw(hwnd: HWND, ui_weak: &slint::Weak<AppWindow>) {
    let ui_weak_clone = ui_weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui_weak_clone.upgrade() {
            ui.show().unwrap();
            ui.window().request_redraw();
            ui.set_refresh_counter(ui.get_refresh_counter() + 1);
        }
    });
    unsafe {
        if !hwnd.0.is_null() {
            ShowWindow(hwnd, SW_SHOW);
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
            let _ = InvalidateRect(hwnd, None, true);
            let _ = UpdateWindow(hwnd);
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if args.debug {
        println!("{}", ">>> AS2P V8 'PULSE' SERVER (DEBUG MODE) <<<".bold().bright_white().on_blue());
    }

    let is_running = Arc::new(AtomicBool::new(false));
    let (ui_stats_tx, ui_stats_rx) = unbounded::<UiMessage>();
    let (cmd_tx, mut cmd_rx) = tokio_mpsc::channel::<UiCommand>(10);
    let is_ui_visible = Arc::new(AtomicBool::new(true));
    let main_tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };

    let (ui_handle_tx, ui_handle_rx) = unbounded::<slint::Weak<AppWindow>>();
    let is_running_ui = is_running.clone();
    let is_ui_visible_ui = is_ui_visible.clone();
    let cmd_tx_ui = cmd_tx.clone();
    std::thread::spawn(move || {
        ui::run_ui(UiState { is_running: is_running_ui, stats_rx: ui_stats_rx, cmd_tx: cmd_tx_ui, is_ui_visible: is_ui_visible_ui, main_thread_id: main_tid, }, ui_handle_tx);
    });
    let ui_weak = ui_handle_rx.recv().expect("Err");

    let ui_stats_tx_server = ui_stats_tx.clone();
    let is_running_server = is_running.clone();
    let is_ui_visible_server = is_ui_visible.clone();
    let debug_mode = args.debug;
    let drop_rate = args.drop_rate;

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let mut state = ServerState::Listening; // Auto-start
            is_running_server.store(true, Ordering::SeqCst); // Sync atomic
            
            // Re-evaluating UI sync: 
            // The UI thread reads `is_running` every 250ms (in run_ui -> timer).
            // So simply setting `is_running` to true and `state` to Listening is enough for UI visual.
            // But we need to make sure the loop logic handles this.
            
            // Re-evaluating UI sync: 
            // The UI thread reads `is_running` every 250ms (in run_ui -> timer).
            // So simply setting `is_running` to true and `state` to Listening is enough for UI visual.
            // But we need to make sure the loop logic handles this.
            
            let mut redundancy = true;
            let mut current_bitrate = 160000;
            let mut current_complexity = 5;

            loop {
                match state {
                    ServerState::Idle => {
                        is_running_server.store(false, Ordering::SeqCst);
                        while let Some(cmd) = cmd_rx.recv().await {
                            match cmd {
                                UiCommand::ToggleServer => { state = ServerState::Listening; break; }
                                UiCommand::SetRedundancy(r) => redundancy = r,
                            }
                        }
                    }
                    ServerState::Listening => {
                        is_running_server.store(true, Ordering::SeqCst);
                        if debug_mode { println!("{} Waiting for AS2P_DISCOVER...", "[CONN]".blue()); }
                        
                        let socket = Arc::new(bind_socket(12345).expect("Err"));
                        let mut buf = [0u8; 1024];
                        let mut target_addr: Option<std::net::SocketAddr> = None;

                        loop {
                            tokio::select! {
                                msg = cmd_rx.recv() => {
                                    match msg {
                                        Some(UiCommand::ToggleServer) => { state = ServerState::Idle; break; }
                                        Some(UiCommand::SetRedundancy(r)) => redundancy = r,
                                        _ => {}
                                    }
                                }
                                result = socket.recv_from(&mut buf) => {
                                    if let Ok((len, addr)) = result {
                                        if len >= 13 && &buf[0..13] == b"AS2P_DISCOVER" {
                                            let fixed_addr = std::net::SocketAddr::new(addr.ip(), 12345);
                                            target_addr = Some(fixed_addr);
                                            let _ = socket.send_to(b"AS2P_OFFER", fixed_addr).await;
                                            if debug_mode { println!("{} Offer sent to {}", "[CONN]".blue(), fixed_addr); }
                                        } else if len >= 16 && &buf[0..11] == b"AS2P_CONFIG" {
                                            current_bitrate = i32::from_le_bytes(buf[11..15].try_into().unwrap());
                                            current_complexity = buf[15] as i32;
                                            state = ServerState::Streaming;
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        
                        if state == ServerState::Streaming && target_addr.is_some() {
                            let target = target_addr.unwrap();
                            let mut udp_sender = UdpSender::new(socket.clone(), target, debug_mode, redundancy, drop_rate);
                            let mut capturer = AudioCapturer::new(debug_mode).expect("Err");
                            let mut encoder = create_encoder().expect("Err");
                            let _ = configure_encoder(&mut encoder, current_bitrate, current_complexity);
                            let mut pcm_buffer = Vec::with_capacity(1920 * 10);
                            let mut sequence = 0u64;
                            let start_time = std::time::Instant::now();
                            let mut last_activity_time = std::time::Instant::now();
                            let mut last_ui_update_time = std::time::Instant::now();

                            loop {
                                tokio::select! {
                                    cmd_msg = cmd_rx.recv() => {
                                        match cmd_msg {
                                            Some(UiCommand::ToggleServer) => { state = ServerState::Stopping; break; }
                                            Some(UiCommand::SetRedundancy(r)) => {
                                                redundancy = r;
                                                udp_sender.redundancy_enabled = r;
                                            }
                                            _ => {}
                                        }
                                    }
                                    result = socket.recv_from(&mut buf) => {
                                        if let Ok((len, _)) = result {
                                            last_activity_time = std::time::Instant::now();
                                            if len >= 10 && &buf[0..10] == b"AS2P_ALIVE" {
                                                // Heartbeat received, activity time updated above
                                                continue;
                                            } else if len >= 16 && &buf[0..11] == b"AS2P_CONFIG" {
                                                let new_bitrate = i32::from_le_bytes(buf[11..15].try_into().unwrap());
                                                let new_complexity = buf[15] as i32;

                                                if new_bitrate == current_bitrate && new_complexity == current_complexity {
                                                    if debug_mode { println!("{} Received redundant config, ignoring.", "[AUDIO]".blue()); }
                                                    continue;
                                                }

                                                // Hot-reload config
                                                if debug_mode { println!("{} Hot-reloading config ({}bps)...", "[AUDIO]".green(), new_bitrate); }
                                                
                                                current_bitrate = new_bitrate;
                                                current_complexity = new_complexity;
                                                
                                                // 2. Fade-out
                                                capturer.start_fade_out();
                                                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                                                
                                                // 3. Re-create Encoder & Reset Capturer (Implicit Fade-in)
                                                encoder = create_encoder().expect("Err");
                                                let _ = configure_encoder(&mut encoder, current_bitrate, current_complexity);
                                                let _ = capturer.reinitialize(); // This resets fader to 0 and fades in to 1
                                                
                                                // 4. Reset Buffer
                                                pcm_buffer.clear();
                                                sequence = 0; // Optional: Resetting seq might cause jump on client, but Config implies restart
                                                
                                                if debug_mode { println!("{} Config Applied: {}bps", "[AUDIO]".green(), current_bitrate); }
                                            } else if len >= 12 && &buf[0..12] == b"AS2P_GOODBYE" {
                                                if debug_mode { println!("{} Client disconnected gracefully.", "[CONN]".blue()); }
                                                state = ServerState::Listening; break;
                                            }
                                        }
                                    }
                                    _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {
                                        if last_activity_time.elapsed().as_secs() > 5 {
                                            if debug_mode { println!("{} Client connection timed out.", "[CONN]".red()); }
                                            state = ServerState::Listening; 
                                            let _ = ui_stats_tx_server.send(UiMessage::UpdateStats { packets: 0, bitrate: 0, client_ip: None });
                                            break; 
                                        }
                                        
                                        if last_ui_update_time.elapsed().as_millis() >= 200 {
                                            if is_ui_visible_server.load(Ordering::SeqCst) {
                                                let _ = ui_stats_tx_server.send(UiMessage::UpdateStats { 
                                                    packets: sequence, 
                                                    bitrate: current_bitrate, 
                                                    client_ip: Some(target.to_string()) 
                                                });
                                            }
                                            last_ui_update_time = std::time::Instant::now();
                                        }

                                        for _ in 0..10 {
                                            match capturer.next_event() {
                                                CaptureEvent::Data(pcm) => {
                                                    if pcm.is_empty() { break; }
                                                    pcm_buffer.extend(pcm);
                                                    while pcm_buffer.len() >= 1920 {
                                                        let frame: Vec<f32> = pcm_buffer.drain(0..1920).collect();
                                                        let data = encode_frame(&mut encoder, &frame);
                                                        if !data.is_empty() {
                                                            let ts = start_time.elapsed().as_millis() as u64;
                                                            let _ = udp_sender.send_audio_v8(sequence, ts, data).await;
                                                            sequence += 1;
                                                        }
                                                    }
                                                }
                                                CaptureEvent::DeviceChanged(name) => {
                                                    if debug_mode { println!("{} Device Switched to {}", "[AUDIO]".green(), name); }
                                                    pcm_buffer.clear();
                                                }
                                                CaptureEvent::Error(e) => {
                                                    if debug_mode { println!("{} Audio Error: {}", "[ERROR]".red(), e); }
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            if state == ServerState::Stopping {
                                if debug_mode { println!("{} Stopping Stream (Graceful)...", "[CONN]".blue()); }
                                capturer.start_fade_out();
                                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                                let _ = udp_sender.send_control("AS2P_GOODBYE").await;
                                state = ServerState::Idle;
                            }
                            let _ = ui_stats_tx_server.send(UiMessage::UpdateStats { packets: 0, bitrate: 0, client_ip: None });
                        }
                    }
                    _ => state = ServerState::Idle,
                }
            }
        });
    });

    let tray_menu = Menu::new();
    let _ = tray_menu.append_items(&[
        &MenuItem::with_id("toggle", "Show/Hide Window", true, None),
        &MenuItem::with_id("server_toggle", "Start/Stop Server", true, None),
        &MenuItem::with_id("quit", "Quit", true, None),
    ]);
    
    let mut _tray = Some(TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_icon(load_embedded_icon())
        .with_tooltip("AS2P Server")
        .build()?);

    let menu_channel = MenuEvent::receiver();
    let tray_channel = TrayIconEvent::receiver();
    let mut msg = MSG::default();
    
    let mut last_tray_state = false;

    unsafe {
        while GetMessageW(&mut msg, HWND(std::ptr::null_mut()), 0, 0).as_bool() {
            TranslateMessage(&msg); DispatchMessageW(&msg);
            
            let current_running = is_running.load(Ordering::SeqCst);
            if current_running != last_tray_state {
                if let Some(ref tray) = _tray {
                    let _ = tray.set_tooltip(Some(if current_running { "AS2P Server (Streaming)" } else { "AS2P Server (Idle)" }));
                }
                last_tray_state = current_running;
            }

            let hwnd = FindWindowW(None, w!("AS2P_SERVER_UI")).unwrap_or(HWND(std::ptr::null_mut()));
            let visible = !hwnd.0.is_null() && IsWindowVisible(hwnd).as_bool();
            is_ui_visible.store(visible, Ordering::SeqCst);
            
            while let Ok(event) = menu_channel.try_recv() {
                match event.id.0.as_str() {
                    "toggle" => { if !hwnd.0.is_null() { if visible { ShowWindow(hwnd, SW_HIDE); } else { force_window_redraw(hwnd, &ui_weak); } } }
                    "server_toggle" => { let _ = cmd_tx.try_send(UiCommand::ToggleServer); }
                    "quit" => {
                        _tray.take(); // 關鍵：先手動移除圖示
                        PostQuitMessage(0); // 讓主迴圈結束
                    }
                    _ => {}
                }
            }
            while let Ok(event) = tray_channel.try_recv() {
                if let TrayIconEvent::DoubleClick { .. } = event { if !hwnd.0.is_null() { force_window_redraw(hwnd, &ui_weak); } }
            }
        }
    }
    Ok(())
}

fn bind_socket(port: u16) -> Result<UdpSocket, String> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).map_err(|e| e.to_string())?;
    socket.set_reuse_address(true).map_err(|e| e.to_string())?;
    let addr: SockAddr = format!("0.0.0.0:{}", port).parse::<std::net::SocketAddr>().map_err(|e| e.to_string())?.into();
    socket.bind(&addr).map_err(|e| e.to_string())?;
    let std_socket: std::net::UdpSocket = socket.into();
    std_socket.set_nonblocking(true).map_err(|e| e.to_string())?;
    UdpSocket::from_std(std_socket).map_err(|e| e.to_string())
}
