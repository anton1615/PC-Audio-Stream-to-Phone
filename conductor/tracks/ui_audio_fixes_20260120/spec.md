# Track Specification: UI Sync, Background Audio & BT Route Fixes

## 1. 概述 (Overview)
本 Track 旨在修復核心連線邏輯與音訊播放的穩定性問題，包含 Server UI 同步、Android 背景播放音質，以及藍牙耳機切換時的自動處理與路由失效修復。

## 2. 功能需求 (Functional Requirements)
*   **Bitrate 即時同步**：
    *   Server 收到 Android 的 Preset 切換指令後，Slint UI 必須即時反應最新的 Bitrate (非固定在 128K)。
*   **Android 背景音訊優化**：
    *   消除 App 在背景或螢幕熄滅時伴隨音訊出現的微小爆裂聲 (Crackle)。
*   **藍牙斷線自動處理 (回歸修復)**：
    *   當 Android 端檢測到藍牙耳機斷開連線時，必須自動執行 `Disconnect` 動作，停止與 Server 的連線。
*   **音訊路由切換修復**：
    *   修復「從手機喇叭切換至藍牙耳機」時會導致兩端皆失去聲音的 Bug。
    *   系統應能在輸出設備變更時自動重新導向音流，或在必要時觸發重新連線邏輯以恢復聲音。

## 3. 非功能需求 (Non-Functional Requirements)
*   **強健性**：系統應能正確處理 Android 系統層級的音訊變更廣播 (Audio Focus / Output Changes)。
*   **即時性**：藍牙斷開後的自動中斷應在 1-2 秒內完成。

## 4. 驗收標準 (Acceptance Criteria)
- [ ] Android 切換 Preset，Server UI 數值即時更新。
- [ ] 背景與熄屏模式下播放 5 分鐘無爆裂聲。
- [ ] **測試 1**：連線中關閉藍牙耳機，Android App 應自動顯示為已斷開連線狀態。
- [ ] **測試 2**：使用喇叭播放時連上藍牙耳機，音訊應自動轉移至耳機（或透過自動重新連線恢復聲音），不再發生靜音。

## 5. 超出範圍 (Out of Scope)
*   更改音訊編碼格式 (Opus 以外)。
