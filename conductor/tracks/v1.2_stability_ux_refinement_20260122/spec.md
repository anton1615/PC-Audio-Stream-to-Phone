# Track Specification: v1.2 Stability & UX Refinement

## 1. 概述 (Overview)
本 Track 旨在解決 AS2P 在長期運作與實際部署中遇到的 UX 痛點，包括 Server 視窗干擾、Android 通知欄行為不直覺、APK 更新衝突，以及切換模式時的音訊斷續問題。

## 2. 需求說明 (Requirements)

### 2.1 Windows Server 優化
- **隱藏主控台 (Console Hiding)**：
    - 預設編譯為 Windows GUI Subsystem，啟動時不顯示 CMD 小黑窗。
    - 若使用者需要除錯，需手動透過 CMD 執行。
- **安全限制 (Security)**：
    - 限制 `AS2P_CONFIG` 接收的 Bitrate 範圍為 16kbps 至 512kbps。
- **日誌顯示 (Logging)**：
    - 維持顯示完整 IP 地址，不進行遮蔽，以便使用者辨識連線裝置。

### 2.2 Android App UX 增強
- **媒體面板整合 (Media Session Integration)**：
    - 實作 MediaSession 支援，整合 Android 媒體控制卡片外觀（Spotify 風格）。
    - 播放中：通知常駐且不可滑除；中斷連線後：通知轉變為可滑除狀態。
- **任務移除行為 (Task Removal)**：
    - 確保在多工列表滑掉 App 時，立刻停止 Service 並中斷連線。
- **APK 自動化配置**：
    - 實作固定數位簽名 (Signing Config)，確保 APK 可覆蓋安裝。
    - 實作 versionCode 自動遞增。

### 2.3 音訊連線魯棒性 (Robustness)
- **伺服器端「熱機封包」 (Warm-up Packets)**：
    - Server 完成模式切換後，立刻主動發送 10 個靜音封包，以確保 Android 端 Socket 喚醒並順利鎖定新的封包序號。

## 3. 驗證標準 (Acceptance Criteria)
1. 點擊 `server.exe` 時不會出現 CMD 視窗。
2. Android 通知欄顯示為媒體卡片格式，且連線中無法滑除。
3. 下載新版 APK 後，可直接點擊「更新」而不會提示安裝失敗。
4. 切換 Preset 時，音訊能在 1.5 秒內恢復正常，不應出現長時間斷訊。
5. 發送超過 512kbps 的 Config 時，Server 應將其限制在最高值。

## 4. 非功能性需求 (Non-Functional Requirements)
- **相容性**：Media Session 必須能在 Android 10+ 正常運作。
- **效能**：隱藏 Console 不應影響 GUI 響應速度。