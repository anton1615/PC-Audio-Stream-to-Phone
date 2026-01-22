# Implementation Plan - v1.2 Stability & UX Refinement

本計畫旨在實作 v1.2 規格書中的所有需求，優化 Server 與 Android 的連線魯棒性與使用者體驗。

## Phase 1: Windows Server 介面與安全優化
- [ ] **Task: 實作 Bitrate 範圍限制**
    - [ ] 在 `server/src/network.rs` 中編寫測試，驗證當接收到超過 512kbps 或低於 16kbps 的 Config 時的處理邏輯。
    - [ ] 修改 `handle_config` 邏輯，強制將 Bitrate 限制在 16-512kbps 區間。
    - [ ] 確保 IP 地址在日誌中維持完整顯示（不進行遮蔽）。
- [ ] **Task: 隱藏主控台視窗 (Console Hiding)**
    - [ ] 修改 `server/src/main.rs`，加入 `#![windows_subsystem = "windows"]` 編譯屬性。
    - [ ] 驗證 GUI 仍能正常啟動且托盤功能運作正常。
- [ ] **Task: Conductor - User Manual Verification 'Phase 1' (Protocol in workflow.md)**

## Phase 2: Android 建置與發佈自動化
- [ ] **Task: 實作固定數位簽名 (Signing Config)**
    - [ ] 在 `android/app/` 下生成或配置 debug 密鑰庫。
    - [ ] 修改 `android/app/build.gradle`，配置 `signingConfigs` 使其在 Debug 與 Release 模式下使用相同密鑰。
- [ ] **Task: 實作 versionCode 自動遞增**
    - [ ] 在 `android/app/build.gradle` 中實作基於時間或自動累加的 `versionCode` 邏輯。
- [ ] **Task: Conductor - User Manual Verification 'Phase 2' (Protocol in workflow.md)**

## Phase 3: Android 媒體面板整合 (Spotify Style)
- [ ] **Task: 整合 MediaSession 支援**
    - [ ] 在 `AudioService.kt` 中初始化 `MediaSessionCompat`。
    - [ ] 設置正確的 PlaybackState（播放中/停止）。
- [ ] **Task: 更新通知欄外觀為媒體卡片**
    - [ ] 使用 `androidx.media.app.NotificationCompat.MediaStyle` 更新通知建構邏輯。
    - [ ] 調整通知屬性，確保播放時不可滑除，停止後可滑除。
- [ ] **Task: 驗證任務移除行為 (Task Removal)**
    - [ ] 確保 `onTaskRemoved` 觸發時能正確關閉 MediaSession 並停止服務。
- [ ] **Task: Conductor - User Manual Verification 'Phase 3' (Protocol in workflow.md)**

## Phase 4: 音訊連線魯棒性 (Warm-up Packets)
- [ ] **Task: Server 端熱機封包實作**
    - [ ] 在 `server/src/network.rs` 中編寫測試，驗證發送 `CONFIG_ACK` 後是否正確觸發 10 個封包的發送。
    - [ ] 實作在完成編碼器重置後，主動發送 10 個靜音封包的邏輯。
- [ ] **Task: Conductor - User Manual Verification 'Phase 4' (Protocol in workflow.md)**

## Phase 5: 最終驗證與清理
- [ ] **Task: 跨平台整合測試**
    - [ ] 驗證從 Android 切換 Preset 時，Server 能穩定切換且音訊恢復迅速。
- [ ] **Task: Conductor - User Manual Verification 'Phase 5' (Protocol in workflow.md)**