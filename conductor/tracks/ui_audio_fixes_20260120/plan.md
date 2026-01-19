# Implementation Plan: UI Sync, Background Audio & BT Route Fixes

## Phase 1: 診斷與通訊協定驗證 (Diagnosis & Protocol)
本階段目標是確認數據包是否正確到達 Server，以及 Android 背景模式下的資源佔用情況。

- [ ] **Task: 診斷 Server 端 Bitrate 接收狀態**
    - [ ] 在 `server/src/network.rs` 增加 log，列印接收到的 `0x02` (Config Update) 封包內容。
    - [ ] 驗證 `server/src/ui.rs` 是否有正確監聽狀態變更並更新 Slint 的 property。
- [ ] **Task: 監控 Android 背景行為**
    - [ ] 使用 Android Profiler 觀察 App 進入背景與螢幕熄滅時的 CPU 佔用與線程優先級。
    - [ ] 檢查 `AudioService.kt` 是否有被系統列入電量優化導致調度延遲。
- [ ] **Task: Conductor - User Manual Verification 'Phase 1' (Protocol in workflow.md)**

## Phase 2: Android 音訊播放與路由修復 (Audio Logic)
修復背景爆裂聲與藍牙切換失效問題。

- [ ] **Task: 優化背景播放穩定性**
    - [ ] 檢查 `Oboe` 的 `PerformanceMode` 設定，確保使用 `LowLatency` 且 `SharingMode` 設為 `Exclusive`。
    - [ ] 考慮在螢幕熄滅時獲取 `PowerManager.WakeLock` (適度) 以維持處理序。
    - [ ] 調整 Jitter Buffer 策略，檢視是否在背景時緩衝區水位不穩。
- [ ] **Task: 修復藍牙斷開自動中斷功能**
    - [ ] 檢查 `ConnectionManager.kt` 中負責監聽藍牙廣播 (`BluetoothDevice.ACTION_ACL_DISCONNECTED`) 的 Receiver。
    - [ ] 重新實作斷開邏輯：當藍牙斷開且目前為連線狀態時，主動調用 `disconnectFromServer()`。
- [ ] **Task: 修復音訊路由切換靜音 Bug**
    - [ ] 監聽 `AudioManager.ACTION_AUDIO_BECOMING_NOISY` 與 `AudioManager.STREAM_DEVICES_CHANGED`。
    - [ ] 確保在設備切換時，`NativeLib` 能夠正確重啟 Oboe Stream 或更新播放 Sink。
- [ ] **Task: Conductor - User Manual Verification 'Phase 2' (Protocol in workflow.md)**

## Phase 3: Server 端 UI 同步修正 (Server-side Fixes)
確保 Server 狀態與 UI 顯示完全同步。

- [ ] **Task: 修復 Bitrate UI 顯示**
    - [ ] 修改 `server/src/ui.rs` 的 message loop，確保當內部狀態變更時，觸發 Slint UI 重繪（使用之前修復白屏時的 `refresh_counter` 邏輯）。
    - [ ] 修正 `server/src/audio.rs` 或 `network.rs` 中處理 Config Update 的邏輯，確保變更已寫入共享狀態。
- [ ] **Task: Conductor - User Manual Verification 'Phase 3' (Protocol in workflow.md)**

## Phase 4: 整合測試 (Verification)
- [ ] **Task: 驗收測試**
    - [ ] 執行 `spec.md` 中定義的四大驗收標準測試。
    - [ ] 確保在修復後，< 1% 的 CPU 佔用目標仍能達成。
- [ ] **Task: Conductor - User Manual Verification 'Phase 4' (Protocol in workflow.md)**
