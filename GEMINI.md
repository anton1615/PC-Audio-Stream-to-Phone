# AS2P Project Context Summary (GEMINI.md)
**Last Updated**: 2026-01-28
**Current Version**: v1.2.1 "Eco Tuner"

## 1. 專案概述 (Project Overview)
AS2P (Audio Stream to Phone) 是一個高效能的 PC 到 Android 音訊串流系統。旨在為無藍牙硬體的 PC 提供低延遲、高穩定性的音訊傳輸方案。

## 2. 核心技術架構 (Core Architecture)

### 2.1 Android Native 層 (C++)
- **解碼解耦 (Decoupled Decoding)**: 獨立的 Worker Thread 處理 Opus 解碼，避免阻塞 Oboe 播放回調。
- **PCM FIFO 緩衝區**: 實作生產者-消費者模型，吸收系統調度抖動。
- **按需解碼 (Demand-based Decoding)**: 僅在 PCM 庫存低於 `mPcmTarget` 時解碼，極大化 Jitter Buffer 空間以對抗網路波動。
- **細粒度鎖 (Fine-grained Locking)**: 耗時解碼在鎖外執行，徹底消除背景爆音。
- **重置冷卻機制**: 100ms 重置期，確保序列號對齊。

### 2.2 Android 應用層 (Kotlin)
- **生命週期感知 (Power Opt)**: 透過 `ACTION_UI_VISIBLE/HIDDEN` 偵測 UI 狀態。在背景播放時停止數據計算與 Flow 推送，最大化 CPU 休眠時間。
- **通訊協定 (Pulse V8)**: UDP Port 12345 歸一化，支援自動探索與心跳。
- **通知管理**: MediaStyle 原生樣式，實作內容快取以減少 redundant IPC 呼叫。
- **自定義 Preset**: 支援完整的參數持久化 (Bitrate, Buffer, PCM Target, Catch-up, PLC)。

### 2.3 Windows Server 層 (Rust)
- **Redundancy 優化**: 1ms 延時雙重發送機制，提升無線環境可靠性。
- **自動重採樣**: 使用 `rubato` 強制轉換為 48kHz。
- **輕量化**: 隱藏控制台窗口，UI 隱藏時停止重繪。

## 3. 版本里程碑 (Milestones)
- **v1.1.5**: 實作 PLC 開關與 1ms Redundancy 延時。
- **v1.2.0 (Tuner)**: 引入「自定義設定」Preset 與進階調校 UI，開放按需解碼參數控制。
- **v1.2.1 (Eco Tuner)**: 深度省電優化，實作 UI 感知節能模式與折疊式介面。

## 4. 關鍵參數建議 (Technical Advice)
- **PCM Pre-decode Target**: 建議值 `2` (40ms)。平衡 CPU 穩定性與 Jitter Buffer 容錯空間。
- **Catch-up Threshold**: 預設 `+1` 以追求極致延遲；網路極差時可調高至 `+5`。
- **Bitrate**: 32k - 320k。32k 已能提供清晰語音，192k 以上接近無損。

## 5. 運行環境 (Environment)
- **測試設備**: Pixel 6a (Tensor 晶片，對背景降頻極其敏感)。
- **開發工具**: Gradle 8.10.2, NDK 25.1, Rust 1.75+, Slint 1.x.

---
**Gemini CLI 指令提醒**: 執行修改前務必檢查 `native-lib.cpp` 的 JNI 簽名一致性，並確保所有 Preset 切換都會重置 Catch-up 與 PCM 參數。
