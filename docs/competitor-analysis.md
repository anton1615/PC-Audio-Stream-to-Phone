# 競品串流 app 分析 vs AS2P 可借鏡點

分析對象與方法:

| 來源 | 取得 | 工具 |
|---|---|---|
| Android 安裝包 | 公開 mirror | androguard 4.0.1(DEX bytecode)、aapt2(manifest) |
| 桌面版安裝檔 | 官網公開下載 | innoextract 1.9、ILSpy 11.1(.NET 元件)、javap |
| 其 native 音訊庫 | 安裝包內 | llvm-objdump 19(NDK r28) |

- 取得的是未加密的原始安裝包,全程使用標準工具,**未規避任何保護措施**。
- **從未執行**對方 app;目的是互操作性研究(比較 AS2P 的傳輸與緩衝設計、判斷有哪些機制值得採用)。
- 本文只保留工程結論(我們採用了什麼、為什麼)。完整分析筆記保存在本機,不進公開 repo。

---

## 1. 關鍵發現(概念層)

到 v1.2.x 為止,AS2P 與對方最大的差距不在編碼器(都用 Opus),而在**可靠性機制**:

| 機制 | 對方做法(概念) | AS2P 當時 |
|---|---|---|
| 可靠性 | **按需重傳**:接收端回報遺失的媒體序號,發送端從快取重送並標記 | 盲目雙送(永遠多花 100% 頻寬) |
| Jitter buffer | **水位控制器**:多個狀態之間轉換,水位可上可下 | 單一「補水中」布林,只會往下砍 |
| 自適應緩衝 | 水位上限**相對於實測佔用**動態調整 | 固定值,無自適應 |
| 時基 | 媒體時間軸與發送時鐘**分離** | 序號 + 時間戳混用,無法正確判斷遲到 |
| 統計 | 接收端量測延遲/遺失/underrun 並回報 | 只有水位深度與 PLC 計數 |
| Windows 排程 | 音訊執行緒註冊 MMCSS | 只有 process priority class |
| Android 電源 | WakeLock + MulticastLock + AudioFocus | 有權限宣告但未使用 |

其他結論:音訊輸出走自製 native 引擎而非 AudioTrack;Android 端有收音方向的 server 角色與降噪;桌面端是 Java UI + .NET 擷取的雙進程架構。這些屬於其內部工程選型,與 AS2P 的改進方向無關。

---

## 2. AS2P 現況總表(分析時 → 本次實作後)

| 面向 | 分析時 AS2P | 本次實作 |
|---|---|---|
| PC 擷取 | `server/src/audio.rs` cpal callback | **未動**(cpal 路線保留) |
| 編碼 | `server/src/encoder.rs` audiopus CBR | **未動**:已到位 |
| 封包頭 | `Seq(8)+TS(8)` | **P1 完成**:v9 `tick(8)+send_ts_ms(8)+flags(8)+seq(4)` |
| 可靠性 | 盲目雙送 | **P4 完成**:`AS2P_RESEND` + 重傳快取+ `FLAG_RETRANSMIT` |
| Jitter buffer | 布林預緩衝 + 追趕 | **P2 完成**:五態水位控制器 |
| 自適應緩衝 | 只有往下砍 | **P3 完成**:上下雙向 + 上限相對實測佔用抬升 |
| 統計回報 | `getBufferDepth` / `getPLCCount` | **P5 完成**:`AS2P_STATS` 客戶端回報 |
| Windows 執行緒 | `HIGH_PRIORITY_CLASS` | **P0 完成**:MMCSS(Pro Audio)thread |
| 電源 | `mediaPlayback` FGS | **P6 完成**:WakeLock + MulticastLock + AudioFocus |
| 協商 | 單向 `AS2P_CONFIG` | **未動**(兩端同版發布,協商解決的是不存在的問題) |

---

## 3. 已實作的改動

### P0 — Windows 音訊執行緒 MMCSS

`server/src/audio.rs`:

- `MmcssGuard`(`AvSetMmThreadCharacteristicsW("Pro Audio")` + `Drop` 內 revert)。
- `register_audio_thread_once()` 以 `thread_local` 在 cpal callback 首次執行時註冊,guard 存活於該執行緒整個生命週期,`Cell<bool>` 短路重複呼叫。
- 理由:`HIGH_PRIORITY_CLASS` 只提高 process 基底優先級,不保證音訊執行緒拿到 MMCSS 保留的排程時槽。

### P1 — 封包頭 v9

```
 0..10   "AS2P_AUDIO"
10..18   media_tick    u64  單調遞增媒體 frame 序號(每 tick = 20 ms)
18..26   send_ts_ms    u64  發送時鐘
26..34   flags         u64  bit0 = FLAG_RETRANSMIT
34..38   seq           u32  發送順序
38..     opus payload
```

`media_tick` 與 `send_ts_ms` 分離是關鍵:遲到與否必須用媒體時間軸判定。重傳包按到達時間算很舊,在媒體時間軸上卻可能完全準時。

### P4 — 按需重傳

- 接收端:解碼執行緒發現序號跳號時,對缺口內最多 8 個 tick 各送一次 `AS2P_RESEND`。
- 發送端:保留最近 128 包;收到請求重送並標記 `FLAG_RETRANSMIT`;重配置編碼器時清快取(避免混到舊 bitrate)。
- 重傳包**不**計入重複封包判定 — 它本來就會重複同一個序號。

### P2/P3 — 自適應 jitter buffer

`android/app/src/main/cpp/adaptive_buffer.h`(純邏輯,可在 host 上測試):

| 狀態 | 進入 | 離開 |
|---|---|---|
| `Normal` | 初始 / 降回基準 | underrun → `PreHolding` |
| `PreHolding` | underrun | 500 ms 無 underrun → `Normal`;再次 underrun → 確認 → `Holding` |
| `Holding` | 確認不穩 | 水位達標且穩定 3000 ms → `PreEmptying` |
| `PreEmptying` | 穩定 | 2000 ms 無 underrun → 降一級;降到底 → `Normal` |
| `Emptying` | 實際丟棄封包 | 水位回到目標 → `PreEmptying` |

上限抬升:當佔用逼近上限時,上限設為「實測佔用 + 固定邊距」而非自我遞增 — 這是讓它**自我限制**的關鍵,否則會無限攀升。單次雜訊 underrun 不得永久抬高水位,這就是確認窗口的用途。

### P5 — 統計

native 新增 `getUnderflowCount` / `getDiscardedFrames` / `getMaxBufferedFrames` / `getTargetFrames` / `getCeilingFrames` / `getLatePercentX100`;客戶端每秒送 `AS2P_STATS + 5×u32`(max/avg latency ms、late%×100、loss%×100、underrun 數);伺服器 `--debug` 列出 `Client[late=…, loss=…, underruns=…]`。

### P6 — Android 電源與層級

`AudioService.kt`:`PARTIAL_WAKE_LOCK` + `WifiManager.MulticastLock`(discovery 廣播在螢幕關閉後仍能收到的前提)+ AudioFocus(僅為取得 media 層級與觀察;**刻意不跟進**對方「焦點被搶走即停串流」——手機端是喇叭,使用者邊連 PC 音訊邊用其他 app 是常規使用情境,焦點被搶只會混音,串流必須繼續);`onDestroy` 依序釋放。Manifest 補 `WAKE_LOCK` 權限。

### 未實作(刻意)

- **WASAPI event-sync 擷取重寫**:對方擷取層有若干穩健性技巧,但 AS2P 走 cpal,替換等於重寫擷取層,收益不匹配風險。
- **雙向特徵協商**:AS2P 兩端同版發布,協商解決的是不存在的問題。
- **protobuf**:固定 38-byte 標頭更省,只需擴充欄位語意。
- **OpenSL/AudioTrack 取代 Oboe**:對方的價值在演算法,已移植到現有 Oboe callback。
- **虛擬音訊驅動、雙進程架構、降噪**:與 AS2P 目標無關。

---

## 4. 驗證

| 項目 | 方式 | 結果 |
|---|---|---|
| Rust 編譯 | `cargo check` | 通過 |
| Rust 單元測試 | `cargo test` | 7 passed(含 v9 佈局、重傳往返、快取逐出) |
| 端到端協定 | `server/tests/protocol_smoke.py` 對真實 server binary | 7/7 通過(discovery、v9 單調性、重傳服務、stats) |
| MMCSS | server `--debug` | `[SYS] Audio thread registered with MMCSS (Pro Audio)` |
| 重傳(線上) | 送 `AS2P_RESEND` | 收到 `FLAG_RETRANSMIT` 封包 |
| 客戶端統計解析 | 送 `AS2P_STATS` | `Client[late=12.34%, loss=0.56%, underruns=7]`,數值與送出一致 |
| 狀態機邏輯 | `android/app/src/test/cpp/adaptive_buffer_test.cpp`(host) | 6 組全通過 |
| Android APK | `:app:assembleDebug` | BUILD SUCCESSFUL,4 ABI |
| CI | tag push → Release Build | 全綠,artifacts 發布 |

#### 實機 runtime 驗證(真實 Android)

以真實 v9 封包餵入 app 接收埠:

```
[Jitter] Buffering Complete. Frames: 8 (target 8)
[Jitter] -> PreEmptying after 5160 ms stable
[Jitter] -> Holding (recovered), watermark 10 frames
[Jitter] ceiling raised 14 -> 16 frames (320 ms, buffered 300 ms)
[Stats] Dupes: 0 (2s), PLC: 6, Underflows: 1, Discarded: 41 (session)
[Jitter] Resend callback registered; retransmission enabled
```

`ceiling raised (buffered 300 ms → 320 ms)` 直接對應上限相對實測佔用 + 固定邊距的邏輯。

### 測試期間發現並修正的實際缺陷

1. **`JNI_OnLoad` 被 C++ name-mangle** → VM 找不到 → `gJvm` 恆為 null → 重傳回呼**完全靜默失效**。最嚴重的缺陷,只有實機驗證才會發現(編譯、單元測試、APK 打包全通過)。修法:移入 `extern "C"`,以 `llvm-readelf --dyn-syms` 確認符號未修飾。
2. `configure()` 讓 ceiling = base,確認 underrun 後無法抬升水位。改為 base + headroom。
3. 上限抬升原為自我遞增,會無限攀升;改為相對實測佔用。
4. 計數器原為破壞性讀取(`exchange(0)`),兩個讀取者互相偷值;改為累積讀取。
5. `getTargetFrames`/`getCeilingFrames` 跨執行緒未加鎖;已加鎖。
6. resend callback 未持鎖讀取;改為持鎖複製、鎖外呼叫。
7. Kotlin 剝掉標頭才傳 native,native 讀不到 tick;改為傳完整 post-prefix 緩衝。
8. `resendTarget` 在 `serverAddress` 清空時未同步;集中到 `clearServerConnection()`。
9. `resendTarget` 只在收到 OFFER 時設定,OFFER 遺失則重傳永久失效;改為收到 AUDIO 也能推斷連線。

### 未完成的驗證

- 重傳的端到端往返:callback 註冊已驗證生效;`nc` 在 emulator 內不可靠,未能穩定製造 tick 缺口觸發 `requestResend`。伺服器側(`AS2P_RESEND` → `FLAG_RETRANSMIT`)已由 `protocol_smoke.py` 驗證。
- emulator 的 user-mode NAT 無法回程 AS2P 的對稱固定埠設計(server 回覆 `來源IP:12345`,NAT 把來源改寫成 loopback),完整網路往返需真機同網段。
