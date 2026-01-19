#![windows_subsystem = "windows"]

use std::sync::Arc;
use std::sync::Mutex;
use tokio::net::UdpSocket;
use tokio::sync::mpsc as tokio_mpsc;
use std::sync::mpsc as std_mpsc;
use std::sync::atomic::{AtomicBool, Ordering};
use tray_icon::{
    menu::{Menu, MenuItem, MenuEvent},
    TrayIconBuilder, TrayIconEvent
};
use socket2::{Socket, Domain, Type, Protocol, SockAddr};

#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE, SW_SHOW, SW_RESTORE};
#[cfg(target_os = "windows")]
use windows::Win32::Foundation::HWND;

mod audio;
mod encoder;
mod network;
mod discovery;
mod ui;

use crate::audio::AudioCapturer;
use crate::encoder::{create_encoder, encode_frame, configure_encoder};
use crate::network::UdpSender;
use crate::discovery::DiscoveryServer;
use crate::ui::{AudioServerApp, UiMessage, UiCommand};

struct ConfigUpdate {
    bitrate: i32,
    complexity: i32,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("--- AS2P Server (PC Audio Steam to Phone) Starting ---");

    let is_running = Arc::new(AtomicBool::new(true)); 
    let is_visible = Arc::new(AtomicBool::new(true));
    let hwnd_store = Arc::new(Mutex::new(None));

    let tray_menu = Menu::new();
    let show_hide_item = MenuItem::with_id("show_hide", "Show/Hide Window", true, None);
    let start_stop_item = MenuItem::with_id("start_stop", "Start/Stop Server", true, None);
    let quit_item = MenuItem::with_id("quit", "Quit", true, None);
    let _ = tray_menu.append_items(&[&show_hide_item, &start_stop_item, &MenuItem::new("---", false, None), &quit_item]);
    
    let (icon_rgba, icon_width, icon_height) = (vec![255, 0, 0, 255], 1, 1);
    let icon = tray_icon::Icon::from_rgba(icon_rgba, icon_width, icon_height).expect("Icon error");
    let _tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_icon(icon)
        .build()?;

    let (ui_tx, ui_rx) = std_mpsc::channel::<UiMessage>();
    let (cmd_tx, mut cmd_rx) = tokio_mpsc::channel::<UiCommand>(10);
    
    let ui_tx_server = ui_tx.clone();
    let is_running_server = is_running.clone();
    let cmd_tx_server = cmd_tx.clone();
    
    tokio::spawn(async move {
        let mut server_active = true;
        let port = 12345;

        loop {
            if !server_active {
                is_running_server.store(false, Ordering::SeqCst);
                println!("[Server] State: STOPPED.");
                while let Some(cmd) = cmd_rx.recv().await {
                    match cmd {
                        UiCommand::ToggleServer => { server_active = true; break; }
                        UiCommand::Quit => { return; }
                        _ => {}
                    }
                }
            }

            println!("[Server] State: STARTING...");
            is_running_server.store(true, Ordering::SeqCst);
            
            let discovery = DiscoveryServer::new();
            let _ = discovery.start_broadcast(port);

            let socket = match bind_socket(port) {
                Ok(s) => Arc::new(s),
                Err(e) => {
                    eprintln!("[Server] Bind error: {}. Retry in 3s.", e);
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    continue;
                }
            };

            let mut target_addr = String::new();
            let mut buf = [0u8; 1024];

            println!("[Server] State: WAITING for client...");
            loop {
                tokio::select! {
                    msg = cmd_rx.recv() => {
                        match msg {
                            Some(UiCommand::ToggleServer) => { server_active = false; break; }
                            Some(UiCommand::Quit) => { return; }
                            _ => {}
                        }
                    }
                    result = socket.recv_from(&mut buf) => {
                        if let Ok((len, addr)) = result {
                            if len >= 12 && &buf[0..12] == b"AS2P_HELLO__" {
                                target_addr = format!("{}:12345", addr.ip());
                                println!("[Server] Client connected: {}", target_addr);
                                let _ = ui_tx_server.send(UiMessage::UpdateStats { 
                                    packets: 0, bitrate: 128000, client_ip: Some(addr.ip().to_string()) 
                                });
                                break;
                            }
                        }
                    }
                }
            }

            if !server_active { continue; }

            println!("[Server] State: STREAMING...");
            let cmd_tx_internal = cmd_tx_server.clone();
            match run_streaming(&socket, &target_addr, &ui_tx_server, &mut cmd_rx, cmd_tx_internal).await {
                Ok(UiCommand::ToggleServer) => { 
                    println!("[Server] Streaming stopped by user.");
                    server_active = false; 
                }
                Ok(UiCommand::ClientDisconnect) => {
                    println!("[Server] Client disconnected. Returning to WAITING.");
                }
                Ok(UiCommand::Quit) => { return; }
                Err(e) => {
                    eprintln!("[Server] Stream error: {}", e);
                }
            }

            let _ = ui_tx_server.send(UiMessage::UpdateStats { 
                packets: 0, bitrate: 128000, client_ip: None 
            });
        }
    });

    let is_running_gui = is_running.clone();
    let is_visible_gui = is_visible.clone();
    let hwnd_store_gui = hwnd_store.clone();
    let cmd_tx_tray = cmd_tx.clone();

    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "AS2P Server",
        native_options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            let is_visible_tray = is_visible_gui.clone();
            let hwnd_store_tray = hwnd_store_gui.clone();
            let cmd_tx_inner = cmd_tx_tray.clone();

            std::thread::spawn(move || {
                let menu_channel = MenuEvent::receiver();
                let tray_channel = TrayIconEvent::receiver();
                loop {
                    if let Ok(event) = menu_channel.try_recv() {
                        match event.id.0.as_str() {
                            "show_hide" => {
                                let new_state = !is_visible_tray.load(Ordering::SeqCst);
                                is_visible_tray.store(new_state, Ordering::SeqCst);
                                #[cfg(target_os = "windows")]
                                if let Ok(store) = hwnd_store_tray.lock() {
                                    if let Some(h) = *store {
                                        unsafe { let _ = ShowWindow(HWND(h as _), if new_state { SW_SHOW } else { SW_HIDE }); }
                                    }
                                }
                                ctx.request_repaint();
                            }
                            "start_stop" => {
                                let _ = cmd_tx_inner.try_send(UiCommand::ToggleServer);
                                ctx.request_repaint();
                            }
                            "quit" => { 
                                let _ = cmd_tx_inner.try_send(UiCommand::Quit);
                                std::thread::sleep(std::time::Duration::from_millis(200));
                                std::process::exit(0); 
                            }
                            _ => {}
                        }
                    }
                    if let Ok(event) = tray_channel.try_recv() {
                        if let TrayIconEvent::DoubleClick { .. } = event {
                            is_visible_tray.store(true, Ordering::SeqCst);
                            #[cfg(target_os = "windows")]
                            if let Ok(store) = hwnd_store_tray.lock() {
                                if let Some(h) = *store {
                                    unsafe { let _ = ShowWindow(HWND(h as _), SW_RESTORE); }
                                }
                            }
                            ctx.request_repaint();
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            });

            Ok(Box::new(AudioServerApp::new(cc, ui_rx, cmd_tx_tray, is_running_gui, is_visible_gui, hwnd_store_gui)))
        }),
    ).map_err(|e| {
        let _ = cmd_tx.try_send(UiCommand::Quit);
        format!("UI Error: {:?}", e)
    })?;

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

async fn run_streaming(
    socket: &Arc<UdpSocket>,
    target_addr: &str,
    ui_tx: &std_mpsc::Sender<UiMessage>,
    cmd_rx: &mut tokio_mpsc::Receiver<UiCommand>,
    cmd_tx_internal: tokio_mpsc::Sender<UiCommand>,
) -> Result<UiCommand, String> {
    let mut udp_sender = UdpSender::new(target_addr).await.map_err(|e| e.to_string())?;
    let mut capturer = AudioCapturer::new().map_err(|e| e.to_string())?;
    let mut encoder = create_encoder().map_err(|e: audiopus::Error| format!("{:?}", e))?;
    let mut current_bitrate = 128000;
    let _ = configure_encoder(&mut encoder, current_bitrate, 5);

    let (config_tx, mut config_rx) = tokio_mpsc::channel::<ConfigUpdate>(10);
    let listener_socket = socket.clone();

    let listener_task = tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        while let Ok((len, _)) = listener_socket.recv_from(&mut buf).await {
            if len >= 1 {
                match buf[0] {
                    0x02 => {
                        let bitrate = i32::from_le_bytes(buf[1..5].try_into().unwrap_or([0; 4]));
                        let complexity = buf[5] as i32;
                        let _ = config_tx.send(ConfigUpdate { bitrate, complexity }).await;
                    }
                    0x03 => {
                        println!("[Server] Received Disconnect command from phone.");
                        let _ = cmd_tx_internal.try_send(UiCommand::ClientDisconnect);
                        break;
                    }
                    _ => {}
                }
            }
        }
    });

    let mut pcm_buffer = Vec::with_capacity(1920 * 10);
    let mut sequence = 0u64;
    let mut device_check_interval = tokio::time::interval(std::time::Duration::from_secs(2));
    
    // 用於偵測靜音補包的計時器
    let mut last_send_time = std::time::Instant::now();
    let frame_duration = std::time::Duration::from_millis(20);

    let result = loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(command) => break Ok(command),
                    None => {}
                }
            }
            cfg = config_rx.recv() => {
                if let Some(config) = cfg {
                    current_bitrate = config.bitrate;
                    let _ = configure_encoder(&mut encoder, config.bitrate, config.complexity);
                    pcm_buffer.clear();
                    let silence = vec![0.0f32; 1920];
                    for _ in 0..15 {
                        let data = encode_frame(&mut encoder, &silence);
                        let _ = udp_sender.send_audio_with_seq(sequence, data).await;
                        sequence += 1;
                    }
                    last_send_time = std::time::Instant::now();
                }
            }
            _ = device_check_interval.tick() => {
                let current_name = capturer.device_name.clone();
                let check_task = tokio::task::spawn_blocking(move || {
                    AudioCapturer::get_current_default_device_name()
                });
                
                if let Ok(new_name) = check_task.await {
                    if new_name != current_name && new_name != "None" {
                        println!("[Server] Device change detected: [{}] -> [{}]", current_name, new_name);
                        match AudioCapturer::new() {
                            Ok(new_capturer) => {
                                capturer = new_capturer;
                                pcm_buffer.clear();
                                println!("[Server] Hot-swapped successfully.");
                            }
                            Err(e) => eprintln!("[Server] Failed to swap: {}", e),
                        }
                    }
                }
            }
            // 每 1ms 檢查一次數據
            _ = tokio::time::sleep(std::time::Duration::from_millis(1)) => {
                let mut data_received = false;
                while let Ok(Some(mut pcm)) = capturer.read_samples() {
                    data_received = true;
                    pcm_buffer.append(&mut pcm);
                    while pcm_buffer.len() >= 1920 {
                        let frame: Vec<f32> = pcm_buffer.drain(0..1920).collect();
                        let data = encode_frame(&mut encoder, &frame);
                        if !data.is_empty() {
                            let _ = udp_sender.send_audio_with_seq(sequence, data).await;
                            sequence += 1;
                            last_send_time = std::time::Instant::now();
                        }
                    }
                }

                // 如果超過 20ms 沒發送過數據，且緩衝區也空了（代表 Windows 靜音中）
                if !data_received && last_send_time.elapsed() >= frame_duration {
                    // 主動補一個靜音幀
                    let silence = vec![0.0f32; 1920];
                    let data = encode_frame(&mut encoder, &silence);
                    let _ = udp_sender.send_audio_with_seq(sequence, data).await;
                    sequence += 1;
                    last_send_time = std::time::Instant::now();
                    
                    // 每 200 包更新一次 UI 統計 (約 4 秒)
                    if sequence % 200 == 0 {
                        let _ = ui_tx.send(UiMessage::UpdateStats { 
                            packets: sequence, bitrate: current_bitrate, client_ip: Some(target_addr.to_string()) 
                        });
                    }
                } else if sequence % 200 == 0 && data_received {
                    // 正常的 UI 更新
                    let _ = ui_tx.send(UiMessage::UpdateStats { 
                        packets: sequence, bitrate: current_bitrate, client_ip: Some(target_addr.to_string()) 
                    });
                }
            }
        }
    };

    listener_task.abort();
    if let Ok(cmd) = &result {
        match cmd {
            UiCommand::ToggleServer | UiCommand::Quit => {
                let _ = udp_sender.send_raw(&[0x03]).await;
                println!("[Server] Sent Disconnect signal to phone.");
            }
            _ => {}
        }
    }
    result
}