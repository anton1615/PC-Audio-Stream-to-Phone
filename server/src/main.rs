#![windows_subsystem = "windows"]

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
    SendMessageW, WM_SETICON, ICON_SMALL, ICON_BIG, LoadIconW, IDI_APPLICATION
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::Graphics::Gdi::{InvalidateRect, UpdateWindow};
use windows::Win32::Foundation::HWND;
use windows::core::{w, PCWSTR};
use crossbeam_channel::{unbounded};
use slint::ComponentHandle;
use image::ImageReader;
use std::io::Cursor;

mod audio;
mod encoder;
mod network;
mod discovery;
mod ui;

use crate::audio::AudioCapturer;
use crate::encoder::{create_encoder, encode_frame, configure_encoder};
use crate::network::UdpSender;
use crate::discovery::DiscoveryServer;
use crate::ui::{UiMessage, UiCommand, UiState, AppWindow};

fn load_embedded_icon() -> Icon {
    let icon_data = include_bytes!("../../as2p.png");
    let img = ImageReader::new(Cursor::new(icon_data))
        .with_guessed_format()
        .expect("Failed to guess icon format")
        .decode()
        .expect("Failed to decode icon")
        .into_rgba8();

    let (width, height) = img.dimensions();
    let rgba = img.into_raw();
    Icon::from_rgba(rgba, width, height).expect("Failed to create tray icon")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let is_running = Arc::new(AtomicBool::new(false));
    let redundancy_enabled = Arc::new(AtomicBool::new(true));
    let (ui_stats_tx, ui_stats_rx) = unbounded::<UiMessage>();
    let (cmd_tx, mut cmd_rx) = tokio_mpsc::channel::<UiCommand>(10);
    let is_ui_visible = Arc::new(AtomicBool::new(true));

    let main_tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };

    // --- Server Thread ---
    let is_running_server = is_running.clone();
    let redundancy_enabled_server = redundancy_enabled.clone();
    let ui_stats_tx_server = ui_stats_tx.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let discovery = DiscoveryServer::new();
            let port = 12345;
            let _ = discovery.start_broadcast(port);
            let mut server_active = true;
            let mut sequence = 0u64;
            loop {
                if !server_active {
                    is_running_server.store(false, Ordering::SeqCst);
                    while let Some(cmd) = cmd_rx.recv().await {
                        match cmd {
                            UiCommand::ToggleServer => {
                                server_active = true;
                                break;
                            }
                            UiCommand::SetRedundancy(enabled) => {
                                redundancy_enabled_server.store(enabled, Ordering::SeqCst);
                                let _ = ui_stats_tx_server.send(UiMessage::SyncRedundancy(enabled));
                            }
                        }
                    }
                }
                is_running_server.store(true, Ordering::SeqCst);
                let socket = match bind_socket(port) { Ok(s) => Arc::new(s), Err(_) => { tokio::time::sleep(std::time::Duration::from_secs(3)).await; continue; } };
                let mut target_addr = String::new();
                let mut buf = [0u8; 1024];
                loop {
                    tokio::select! {
                        msg = cmd_rx.recv() => { 
                            if let Some(cmd) = msg { 
                                match cmd {
                                    UiCommand::ToggleServer => { server_active = false; break; }
                                    UiCommand::SetRedundancy(enabled) => {
                                        redundancy_enabled_server.store(enabled, Ordering::SeqCst);
                                        let _ = ui_stats_tx_server.send(UiMessage::SyncRedundancy(enabled));
                                    }
                                }
                            } 
                        }
                        result = socket.recv_from(&mut buf) => { if let Ok((len, addr)) = result { if len >= 12 && &buf[0..12] == b"AS2P_HELLO__" { target_addr = format!("{}:12345", addr.ip()); break; } } }
                    }
                }
                if !server_active { continue; }
                let mut udp_sender = match UdpSender::new(&target_addr).await { Ok(s) => s, Err(_) => continue };
                udp_sender.redundancy = redundancy_enabled_server.load(Ordering::SeqCst);
                
                let mut capturer = match AudioCapturer::new() { Ok(c) => c, Err(_) => continue };
                let mut encoder = match create_encoder() { Ok(e) => e, Err(_) => continue };

                let mut current_bitrate = 128000;
                let mut current_complexity = 5;
                let _ = configure_encoder(&mut encoder, current_bitrate, current_complexity);

                let mut pcm_buffer = Vec::with_capacity(1920 * 10);
                loop {
                    tokio::select! {
                        cmd_msg = cmd_rx.recv() => { 
                            if let Some(cmd) = cmd_msg { 
                                match cmd {
                                    UiCommand::ToggleServer => { server_active = false; break; }
                                    UiCommand::SetRedundancy(enabled) => {
                                        redundancy_enabled_server.store(enabled, Ordering::SeqCst);
                                        udp_sender.redundancy = enabled;
                                        let _ = ui_stats_tx_server.send(UiMessage::SyncRedundancy(enabled));
                                    }
                                }
                            } 
                        }

                        // --- Receive Control Packets (0x02: Config, 0x03: Disconnect) ---
                        result = socket.recv_from(&mut buf) => {
                            if let Ok((len, _addr)) = result {
                                if len >= 6 && buf[0] == 0x02 {
                                    let mut bitrate = i32::from_le_bytes(buf[1..5].try_into().unwrap());
                                    let complexity = buf[5] as i32;

                                    if bitrate < 16000 { bitrate = 16000; }
                                    if bitrate > 512000 { bitrate = 512000; }
                                    if complexity < 0 { complexity = 0; }
                                    if complexity > 10 { complexity = 10; }

                                    if let Ok(_) = configure_encoder(&mut encoder, bitrate, complexity) {
                                        current_bitrate = bitrate;
                                        let _ = ui_stats_tx_server.send(UiMessage::UpdateStats {
                                            packets: sequence,
                                            bitrate: current_bitrate,
                                            client_ip: Some(target_addr.clone())
                                        });
                                    }
                                } else if len >= 1 && buf[0] == 0x03 {
                                    let _ = ui_stats_tx_server.send(UiMessage::UpdateStats {
                                        packets: 0,
                                        bitrate: 128000,
                                        client_ip: None
                                    });
                                    break;
                                }
                            }
                        }

                        _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {
                            while let Ok(Some(mut pcm)) = capturer.read_samples() {
                                pcm_buffer.append(&mut pcm);
                                while pcm_buffer.len() >= 1920 {
                                    let frame: Vec<f32> = pcm_buffer.drain(0..1920).collect();
                                    let data = encode_frame(&mut encoder, &frame);
                                    if !data.is_empty() { let _ = udp_sender.send_audio_with_seq(sequence, data).await; sequence += 1; }
                                }
                            }
                            if sequence > 0 && sequence % 500 == 0 {
                                let _ = ui_stats_tx_server.send(UiMessage::UpdateStats {
                                    packets: sequence,
                                    bitrate: current_bitrate,
                                    client_ip: Some(target_addr.clone())
                                });
                            }
                        }
                    }
                }
                let _ = ui_stats_tx_server.send(UiMessage::UpdateStats {
                    packets: 0,
                    bitrate: 128000,
                    client_ip: None
                });
            }
        });
    });

    // --- UI Thread ---
    let (ui_handle_tx, ui_handle_rx) = unbounded::<slint::Weak<AppWindow>>();
    let is_running_ui = is_running.clone();
    let is_ui_visible_ui = is_ui_visible.clone();
    let cmd_tx_ui = cmd_tx.clone();
    std::thread::spawn(move || {
        ui::run_ui(UiState {
            is_running: is_running_ui,
            stats_rx: ui_stats_rx,
            cmd_tx: cmd_tx_ui,
            is_ui_visible: is_ui_visible_ui,
            main_thread_id: main_tid,
        }, ui_handle_tx);
    });

    let ui_weak = ui_handle_rx.recv().expect("UI handle error");

    // --- Tray Loop ---
    let tray_menu = Menu::new();
    let toggle_item = MenuItem::with_id("toggle", "Hide Window", true, None);
    let server_item = MenuItem::with_id("server_toggle", "Start Server", true, None);
    let redundancy_item = MenuItem::with_id("redundancy_toggle", "Enable Redundancy", true, None);
    let quit_item = MenuItem::with_id("quit", "Quit", true, None);
    let _ = tray_menu.append_items(&[
        &toggle_item,
        &server_item,
        &redundancy_item,
        &MenuItem::new("---", false, None),
        &quit_item
    ]);

    let tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_icon(load_embedded_icon())
        .with_tooltip("AS2P Audio Server")
        .build()?;

    let menu_channel = MenuEvent::receiver();
    let tray_channel = TrayIconEvent::receiver();

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, HWND(std::ptr::null_mut()), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);

            let hwnd = FindWindowW(None, w!("AS2P_SERVER_UI")).unwrap_or(HWND(std::ptr::null_mut()));
            let visible = !hwnd.0.is_null() && IsWindowVisible(hwnd).as_bool();
            is_ui_visible.store(visible, Ordering::SeqCst);

            // Force set icon if window is found
            if !hwnd.0.is_null() {
                let h_instance = GetModuleHandleW(None).unwrap();
                // Load the icon from the executable resources (ID 1 is default for winres)
                let h_icon = LoadIconW(h_instance, PCWSTR(1 as *const u16)).unwrap_or_else(|_| {
                    LoadIconW(None, IDI_APPLICATION).unwrap()
                });
                SendMessageW(hwnd, WM_SETICON, windows::Win32::Foundation::WPARAM(ICON_SMALL as usize), windows::Win32::Foundation::LPARAM(h_icon.0 as isize));
                SendMessageW(hwnd, WM_SETICON, windows::Win32::Foundation::WPARAM(ICON_BIG as usize), windows::Win32::Foundation::LPARAM(h_icon.0 as isize));
            }

            let redundancy = redundancy_enabled.load(Ordering::SeqCst);

            let _ = toggle_item.set_text(if visible { "Hide Window" } else { "Show Window" });
            let _ = server_item.set_text(if is_running.load(Ordering::SeqCst) { "Stop Server" } else { "Start Server" });
            let _ = redundancy_item.set_text(if redundancy { "\u{2713} Enable Redundancy" } else { "Enable Redundancy" });

            while let Ok(event) = menu_channel.try_recv() {
                match event.id.0.as_str() {
                    "toggle" => {
                        if !hwnd.0.is_null() {
                            if visible {
                                let _ = ShowWindow(hwnd, SW_HIDE);
                            } else {
                                let _ = ShowWindow(hwnd, SW_SHOW);
                                let _ = ShowWindow(hwnd, SW_RESTORE);
                                let _ = SetForegroundWindow(hwnd);
                                let _ = InvalidateRect(hwnd, None, false);
                                let _ = UpdateWindow(hwnd);

                                let ui_weak_clone = ui_weak.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = ui_weak_clone.upgrade() {
                                        let old = ui.get_refresh_counter();
                                        ui.set_refresh_counter(old + 1);
                                        ui.window().request_redraw();
                                    }
                                });
                            }
                        }
                    }
                    "server_toggle" => { let _ = cmd_tx.try_send(UiCommand::ToggleServer); }
                    "redundancy_toggle" => {
                        let current = redundancy_enabled.load(Ordering::SeqCst);
                        let _ = cmd_tx.try_send(UiCommand::SetRedundancy(!current));
                    }
                    "quit" => { drop(tray_icon); std::process::exit(0); }
                    _ => {}
                }
            }
            while let Ok(event) = tray_channel.try_recv() {
                if let TrayIconEvent::DoubleClick { .. } = event {
                    if !hwnd.0.is_null() {
                        let _ = ShowWindow(hwnd, SW_SHOW);
                        let _ = ShowWindow(hwnd, SW_RESTORE);
                        let _ = SetForegroundWindow(hwnd);
                        let _ = InvalidateRect(hwnd, None, false);
                        let _ = UpdateWindow(hwnd);

                        let ui_weak_clone = ui_weak.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(ui) = ui_weak_clone.upgrade() {
                                let old = ui.get_refresh_counter();
                                ui.set_refresh_counter(old + 1);
                                ui.window().request_redraw();
                            }
                        });
                    }
                }
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
