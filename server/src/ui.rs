use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc as tokio_mpsc;
use crossbeam_channel::Receiver as CrossbeamReceiver;
use slint::ComponentHandle;
use windows::Win32::UI::WindowsAndMessaging::{PostThreadMessageW, WM_NULL, FindWindowW, ShowWindow, SW_HIDE};
use windows::core::w;

slint::slint! {
    import { Button, VerticalBox, HorizontalBox } from "std-widgets.slint";

    export component AppWindow inherits Window {
        in property <string> status_text: "Idle";
        in property <bool> is_streaming: false;
        in property <string> bitrate_text: "128000 bps";
        in property <string> packets_text: "0 packets";
        in property <string> client_text: "None";
        in property <int> refresh_counter: 0;
        in-out property <bool> redundancy_enabled: true;

        callback toggle_server();
        callback toggle_redundancy(bool);

        title: "AS2P_SERVER_UI";
        icon: @image-url("../../as2p.png");
        width: 400px;
        height: 350px;
        background: #1e1e1e;

        // [CRITICAL FIX] ?魂遴唳??? refresh_counter
        // ?謕?擗??賃祗 counter ?撖??蹇???皜蜃????        Rectangle {
            width: 100%;
            height: 100%;
            background: root.refresh_counter >= 0 ? #1e1e1e : #1e1e1e;

            VerticalLayout {
                padding: 25px;
                spacing: 12px;
                Text { text: "AS2P Server"; font-size: 24px; color: white; horizontal-alignment: center; }
                HorizontalLayout {
                    alignment: center;
                    spacing: 8px;
                    Text { text: "Status:"; color: #aaaaaa; font-size: 16px; }
                    Text { text: root.status_text; color: root.is_streaming ? #00ff00 : #ff5555; font-size: 16px; font-weight: 700; }
                }
                Button { text: root.is_streaming ? "Stop Server" : "Start Server"; height: 40px; clicked => { root.toggle_server(); } }
                
                HorizontalLayout {
                    alignment: center;
                    spacing: 10px;
                    Text { text: "Enable Redundancy (Double Send):"; color: #cccccc; font-size: 14px; vertical-alignment: center; }
                    // Simple checkbox-like behavior using a Rectangle and TouchArea since Slint std CheckBox might not be styled consistently here
                    Rectangle {
                        width: 20px;
                        height: 20px;
                        background: root.redundancy_enabled ? #00ff00 : #444444;
                        border-radius: 4px;
                        TouchArea {
                            clicked => { 
                                root.redundancy_enabled = !root.redundancy_enabled;
                                root.toggle_redundancy(root.redundancy_enabled);
                            }
                        }
                        Text { text: root.redundancy_enabled ? "✔" : ""; color: black; font-size: 14px; horizontal-alignment: center; vertical-alignment: center; }
                    }
                }

                Rectangle { height: 1px; background: #333333; }
                VerticalLayout {
                    spacing: 4px;
                    Text { text: "Bitrate: " + root.bitrate_text; color: #888888; font-size: 14px; }
                    Text { text: "Sent: " + root.packets_text; color: #888888; font-size: 14px; }
                    Text { text: "Client: " + root.client_text; color: #888888; font-size: 14px; }
                }
                Text { text: "Tip: Close window (X) to hide to tray."; font-size: 12px; color: #555555; horizontal-alignment: center; vertical-alignment: bottom; }
            }
        }
    }
}

pub enum UiMessage {
    UpdateStats { packets: u64, bitrate: i32, client_ip: Option<String> },
    SyncRedundancy(bool),
}

#[derive(Clone, Copy, Debug)]
pub enum UiCommand { 
    ToggleServer,
    SetRedundancy(bool),
}

pub struct UiState {
    pub is_running: Arc<AtomicBool>,
    pub stats_rx: CrossbeamReceiver<UiMessage>,
    pub cmd_tx: tokio_mpsc::Sender<UiCommand>,
    pub is_ui_visible: Arc<AtomicBool>,
    pub main_thread_id: u32,
}

pub fn run_ui(state: UiState, handle_tx: crossbeam_channel::Sender<slint::Weak<AppWindow>>) {
    unsafe { std::env::set_var("SLINT_BACKEND", "software"); }
    let window = AppWindow::new().expect("Failed to create Slint window");
    
    let _ = handle_tx.send(window.as_weak());

    let is_visible_on_close = state.is_ui_visible.clone();
    let main_tid = state.main_thread_id;
    window.window().on_close_requested(move || {
        if let Ok(hwnd) = unsafe { FindWindowW(None, w!("AS2P_SERVER_UI")) } {
            if !hwnd.0.is_null() {
                unsafe { let _ = ShowWindow(hwnd, SW_HIDE); }
            }
        }
        is_visible_on_close.store(false, Ordering::SeqCst);
        unsafe { let _ = PostThreadMessageW(main_tid, WM_NULL, None, None); }
        slint::CloseRequestResponse::KeepWindowShown
    });

    let cmd_tx_ui = state.cmd_tx.clone();
    window.on_toggle_server(move || { let _ = cmd_tx_ui.try_send(UiCommand::ToggleServer); });

    let cmd_tx_redundancy = state.cmd_tx.clone();
    window.on_toggle_redundancy(move |enabled| { let _ = cmd_tx_redundancy.try_send(UiCommand::SetRedundancy(enabled)); });

    let window_weak = window.as_weak();
    let is_running_timer = state.is_running.clone();
    let stats_rx_timer = state.stats_rx.clone();
    let timer = slint::Timer::default();

    // ?豲暑?賹?鞎赤?counter
    window.set_refresh_counter(1);

    timer.start(slint::TimerMode::Repeated, std::time::Duration::from_millis(250), move || {
        if let Some(ui) = window_weak.upgrade() {
            // [IMPORTANT] ?潘撓貔 is_visible?伐????            let running = is_running_timer.load(Ordering::SeqCst
);
            ui.set_is_streaming(running);
            ui.set_status_text(if running { "Streaming".into() } else { "Idle".into() });
            while let Ok(msg) = stats_rx_timer.try_recv() {
                match msg {
                    UiMessage::UpdateStats { packets, bitrate, client_ip } => {
                        ui.set_packets_text(format!("{} packets", packets).into());
                        ui.set_bitrate_text(format!("{} bps", bitrate).into());
                        ui.set_client_text(client_ip.unwrap_or_else(|| "None".to_string()).into());
                    }
                    UiMessage::SyncRedundancy(enabled) => {
                        ui.set_redundancy_enabled(enabled);
                    }
                }
            }
        }
    });

    window.run().expect("Slint event loop error");
}
