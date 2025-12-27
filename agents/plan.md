# SerialMonitorEssential 設計仕様書

## 1. プロジェクト概要

FPGAや高速センサ開発に耐えうる、**12Mbps級の高速シリアル通信**に対応したデスクトップアプリケーション。
Rustの `serialport` クレートによるクロスプラットフォーム対応で「データの完全性（取りこぼしゼロ）」を保証しつつ、Tauri (React) を用いて「モダンで応答性の高いUI」を提供する。

## 2. 要求仕様 (Requirements)

### 2.1. 通信・制御 (Backend)

* **高速通信:** 12Mbps などの非標準・高速ボーレートに対応（OSおよびドライバ許容最大値まで）。
* **データ完全性:** 受信データは内部で1バイトも取りこぼさず保持・ログ保存する。UI描画遅延によるバッファ溢れを防ぐ。
* **ホットプラグ:** USBケーブルの抜き差しを検知し、適切に切断・再接続待機状態へ遷移する。（実装方法はOS依存: WindowsはWM_DEVICECHANGE、Linux/macOSはlibudev/IOKit等）
* **複数起動:** アプリの複数起動を許可し、それぞれで別のCOMポートを監視可能にする。
* **受信エラー処理:** フレーミングエラー、パリティエラー等の受信エラーは無視してデータ受信を継続する。

### 2.2. データ管理・ログ (Data & Logging)

* **全データ保持:** 受信開始から終了までの全データを閲覧可能にする（メモリが許す限りではなく、ディスクを活用して制限を撤廃）。
* **隠蔽されたキャッシュ:** 一時データはシステムの一時ディレクトリ（`std::env::temp_dir()`）配下の `SerialMonitorEssential/<PID>` 領域のファイルに逃がし、アプリ終了時に自動削除する（ユーザーのディスクを汚さない）。PIDを用いることで複数インスタンス間のファイル名衝突を回避する。
* **ログ保存:**
  * ユーザー操作による任意のタイミングでのファイルエクスポート。
  * 保存形式はバイナリ。



### 2.3. ユーザーインターフェース (Frontend)

* **ビューア:**
* **Text Mode:** ASCII表示。CR/LF/NULL等の制御文字を可視化。
* **Hex Mode:** バイナリ（16進数）表示。
* **Hybrid:** 上記の切り替え、または併用。


* **スクロール:** 仮想スクロール（Virtual Scrolling）により、数GBのデータであってもスムーズに閲覧可能にする。
* **グラフ描画:** リアルタイムでのプロット機能（データはUI側で間引いて描画）。
* **送信機能:**
* テキスト/バイナリ入力対応。
* Enterキーの挙動設定（送信/改行）。
* 送信履歴（Up Arrowキーでの呼び出し）。



### 2.4. クロスプラットフォーム対応

本アプリケーションは **Windows / Linux / macOS** で動作するよう設計されている。

| 機能 | 使用クレート | 備考 |
|------|-------------|------|
| シリアルポート操作 | `serialport` | DTR/RTS信号の有効化が必須 |
| ポート列挙 | `serialport::available_ports()` | USB/PCI/Bluetooth対応 |
| プロセス確認 | `sysinfo` | 一時ファイルクリーンアップ用 |
| 一時ディレクトリ | `std::env::temp_dir()` | OS依存パスを自動解決 |
| PID取得 | `std::process::id()` | 全OS共通 |

> [!IMPORTANT]
> **DTR/RTS 信号の有効化:**
> Arduino等のUSB-Serialデバイスはデータ送信にDTR (Data Terminal Ready) / RTS (Request to Send) 信号が必要です。
> ポートオープン後に `write_data_terminal_ready(true)` と `write_request_to_send(true)` を呼び出すこと。

---

## 3. システムアーキテクチャ

システムは大きく **Backend (Rust)**、**Bridge (IPC)**、**Frontend (React)** の3層で構成される。

### 3.1. コンポーネント図 (Chunk-based Data Flow / バケツリレー)

```text
[ 空きバケツ置き場 (Object Pool) ] 
                         ↓ (空のチャンクを取得 / 枯渇時は新規作成)
                 [受信スレッド (Producer)] 
                         ↓ (Arc::new(chunk): 16ms or 満杯で確定)
                 [ 完了チャンクリスト (Shared List) ] <=== [UIスレッド (Reader)]
                         ↓ (参照後に削除)                  (最新データをここから参照)
                 [ロギングスレッド (Consumer)]
                         ↓ (ディスクへ書き込み)
                 [ TEMPファイル / ログファイル ] <====== [UIスレッド (Reader)]
                      (ページング完了)                   (過去ログはここから参照)
```

> [!IMPORTANT]
> **設計の核心:** UIは `finished_list` を **直接参照** する。Loggerのディスク書き込み完了を待たない。

### 3.2. スレッド設計 (Producer-Consumer Model)

データ競合を回避し、高速なスワップを実現するために「オブジェクトプール」と「RwLock による共有リスト」を採用する。

1.  **Serial Worker Thread (Producer):**
    *   **役割:** COMポートからの超高速受信。
    *   **挙動:** `Object Pool` から空のチャンクを取得し（枯渇時は新規作成）、データを書き込む。
    *   **スワップ条件:**
        *   **容量限界:** チャンクが満杯になった時。
        *   **タイムアウト:** 前回のスワップから **16ms (約60fps)** 経過し、かつデータが存在する時。
    *   **排他:** データのコピーやロック待ちは発生しない（チャンクの所有権移動のみ）。
    *   **重要:** 16ms経ってもデータが0バイトならスワップしない（空チャンクの大量生成を防止）。

2.  **Logging/Paging Thread (Consumer):**
    *   **役割:** データの永続化。
    *   **挙動:** `finished_list` の **先頭** チャンクを参照し、ディスクへ書き込む。
    *   **処理順序（データ完全性保証）:**
        1. `front()` で先頭チャンクを参照（pop しない）
        2. ディスクへ追記書き込み
        3. `archived_index` を更新（ここでデータが検索可能になる）
        4. `pop_front()` でチャンクを削除
    *   **終了処理:** 通常処理と同じ順序で残りチャンクを処理。書き込み失敗時もログに記録して次へ進む（無限ループ防止）。
    *   **メモリ管理:** `Arc<Chunk>` の参照カウントが0になった時点でメモリ解放。プールへの返却は行わない（ゼロコピー設計）。
    *   **UI参照保護:** `Arc` により、UIが参照中のチャンクは自動的に保護される。

3.  **UI Thread (Reader):**
    *   **役割:** リアルタイム表示と過去データスクロール。
    *   **リアルタイム表示:** `data-update` イベントで `total_bytes` を受信し、`get_display_rows` APIで表示データを取得。
    *   **過去データ表示:** スクロール位置に応じて `get_display_rows` APIで該当範囲を取得。
    *   **60fpsタイマー:** UiNotifier Threadが約16ms間隔でイベント発火。

---

## 4. データ構造とメモリ管理 (Chunk-based Circular Buffering)

メモリ割り当て（malloc/free）のオーバーヘッドを排除し、GCのないRustの利点を活かした「チャンク循環システム」を採用する。

### 4.1. データ構造

```rust
struct Chunk {
    buffer: Box<[u8]>,    // データ領域 (固定サイズ: 64KB)
    capacity: usize,      // 最大サイズ
    valid_len: usize,     // 有効データ長
    timestamp: u64,       // 受信開始時刻
    global_offset: u64,   // このチャンクの開始オフセット（通信全体での位置）
}

struct DataStore {
    // 空きチャンクのプール (ロックフリーキュー)
    // Worker のみが pop（プール枯渇時は新規作成）
    // ※ Arc<Chunk> 採用により Logger からの返却は行わない
    free_pool: Arc<SegQueue<Chunk>>,
    
    // 受信完了したチャンクのリスト (UI読み取り可能)
    // ★重要: VecDeque を使用し、UIからの読み取り（iter）とLoggerからのpop_frontを両立
    // ★メモリ: Arc drop 時にメモリ解放。Logger追従中は数MB程度で安定
    finished_list: Arc<RwLock<VecDeque<Arc<Chunk>>>>,
    
    // ディスク書き出し完了したデータのインデックス
    archived_index: Arc<RwLock<Vec<PageMetadata>>>,
}

struct PageMetadata {
    file_path: PathBuf,   // Tempファイルパス
    file_offset: u64,     // ファイル内のオフセット
    data_length: usize,   // データ長
    global_offset: u64,   // 通信全体における開始バイト位置
}
```

> [!WARNING]
> **SegQueue vs VecDeque の選択理由:**
> - `SegQueue`: ロックフリーだが pop のみ可能（UI読み取り不可）
> - `VecDeque + RwLock`: UI読み取り（iter）とLogger取り出し（pop_front）を両立

* **Chunk Size:** 12Mbps (約1.5MB/s) において 16ms 分のデータは約24KB。ゆとりを持たせて **64KB** とする。

### 4.2. 読み書きフロー (The Bucket Relay)

1.  **初期化:**
    *   起動時に `Chunk` を一定数（例: 100個 = 約6.4MB）生成し、`free_pool` に投入。
    *   プール枯渇時は新規 `Chunk` を作成（Logger 追従中は稀）。

2.  **Write (受信フェーズ) - Worker Thread:**
    *   `free_pool` から空チャンクを1つ取り出す（なければ新規作成）。
    *   シリアルデータを書き込む。
    *   **16ms経過** または **満杯** になったら `Arc::new(chunk)` で `finished_list` の末尾へ追加（Publish）。
    *   **空データは追加しない**（16ms経ってもデータ0バイトならスワップしない）。

3.  **Persist (保存フェーズ) - Logger Thread:**
    *   `finished_list` の **先頭** を `front()` で参照（まだ削除しない）。
    *   ディスクへ追記書き込み。
    *   書き込み完了後、`archived_index` を更新。
    *   **最後に** `pop_front()` でチャンクを削除（Arc 参照カウント減少 → 0 でメモリ解放）。

4.  **Read (UI/Viewer) - UI Thread via API:**
    *   **リアルタイム表示:** `data-update` イベントで `total_bytes` を受信 → `get_display_rows` APIで表示データを取得。
    *   **過去データ表示:** スクロール位置に応じて `get_display_rows` APIでバックエンドからデータ取得。
    *   **バックエンド処理:** Hex/ASCII変換はRust側で実行、フロントエンドは整形済みデータを受け取る。
    *   **データ検索順序:** `get_data` は `archived_index`（確定データ）を先に検索し、足りない部分を `finished_list`（最新データ）から取得。これにより境界をまたぐリクエストにも正しく対応。

### 4.3. UIへのデータ通知 (Push Model)

ポーリングではなく、Backend → Frontend への **Push 型通知** を採用する。

**UiNotifier Thread:**
*   `DataStore::total_bytes()` を監視し、最大60fpsに間引いてイベントを発火。
*   イベントには `total_bytes` のみを含む（データ本体は `get_display_rows` で取得）。

```rust
// data-update イベントのペイロード
struct DataUpdatePayload {
    total_bytes: u64,  // 受信済み総バイト数
}
```

### 4.3.1. バックエンド駆動データ表示 (Backend-Driven Pagination)

**問題点:** 仮想スクロールライブラリ（virtua等）は表示範囲を絞るが、全行の配列をフロントエンドで生成する必要があり、大量データ（32MB = 200万行）でOOMが発生する。

**解決策:** **バックエンド（Rust）で表示範囲のデータを絞り込み、必要な行データのみをフロントエンドに送信する。**

#### データフローモデル

```
[Frontend]                              [Backend (Rust)]
     |                                        |
     |-- open_port --------------------------->|
     |                                        |
     |<-- data-update { total_bytes } ---------|  (60fps間引き)
     |                                        |
     |-- get_display_rows(start_row, count) -->|  (スクロール時)
     |                                        |
     |<-- DisplayRowsPayload { rows: [...] } --|  (表示用行データ)
```

#### 新規API: `get_display_rows`

```rust
#[derive(Clone, serde::Serialize)]
struct DisplayRow {
    offset: u64,      // 行の開始オフセット
    hex: String,      // "00 01 02 ... 0F"
    ascii: String,    // "Hello World....."
}

#[derive(Clone, serde::Serialize)]
struct DisplayRowsPayload {
    rows: Vec<DisplayRow>,
    total_rows: u64,
}

#[tauri::command]
fn get_display_rows(
    state: State<'_, SerialState>,
    start_row: u64,
    row_count: u32,
) -> Result<DisplayRowsPayload, String> {
    // バックエンドで:
    // 1. start_row * 16 ~ (start_row + row_count) * 16 のバイト範囲を計算
    // 2. get_data(offset, length) でデータ取得
    // 3. Hex/ASCII変換してDisplayRow配列を返す
}
```

#### フロントエンドの変更

```tsx
// HexViewer.tsx
const [visibleRows, setVisibleRows] = useState<DisplayRow[]>([]);
const [totalRows, setTotalRows] = useState(0);

// スクロール位置が変わったらバックエンドからデータ取得
const handleScroll = async (e) => {
    const startRow = Math.floor(e.currentTarget.scrollTop / ROW_HEIGHT);
    const { rows, total_rows } = await invoke('get_display_rows', {
        startRow,
        rowCount: VISIBLE_ROWS + BUFFER,
    });
    setVisibleRows(rows);
    setTotalRows(total_rows);
};

// totalRows は total_bytes / 16 で計算（バックエンドで提供）
// スクロールバーの高さ = totalRows * ROW_HEIGHT（上限あり）
// 実際にレンダリングするのは visibleRows のみ
```

#### スケーリング仮想スクロール（実装済み）

ブラウザのDOM最大高さ制限（~33M px）を超えるデータ（32MB = 2M行 = 40M px）を処理するため、スケーリングを導入：

```tsx
const MAX_SCROLL_HEIGHT = 10_000_000; // 10M px
const THROTTLE_MS = 100; // 更新頻度制限

// スケール計算
const scale = Math.min(1, MAX_SCROLL_HEIGHT / (totalRows * ROW_HEIGHT));
const scrollHeight = scale === 1 ? totalRows * ROW_HEIGHT : MAX_SCROLL_HEIGHT;

// Scaled モードでは行を scrollTop 位置に配置（viewport 相対）
const displayTop = scale === 1 ? currentStartRow * ROW_HEIGHT : scrollTop;
```

**実装の詳細:**
- **スロットリング:** Manual mode は 100ms、Auto-scroll は 50ms 間隔で更新
- **スケール同期:** スケール変更時に `scrollTop` を再計算して行を表示領域内に維持
- **Viewport 相対配置:** Scaled モードでは `displayTop = scrollTop` で行を配置

#### メリット

1. **メモリ効率:** フロントエンドは常に~35行分のデータのみ保持
2. **OOM解消:** 全行配列（200万要素）を生成しない
3. **Hex/ASCII変換のオフロード:** Rust側で高速に処理
4. **DOM制限回避:** スケーリングで2M行以上も正常表示
5. **高速受信時の安定性:** スロットリングで React 負荷を軽減
6. **将来の拡張:** 検索、フィルタリング等もバックエンドで処理可能

### 4.4. リスク管理 (Disk I/O Bottleneck)

*   **リスク:** ディスク書き込み速度が受信速度 (12Mbps) を下回ると、`free_pool` が枯渇する。
*   **対策:** 十分な数のチャンク（数秒〜数十秒分）を確保し、一時的な遅延を吸収する。

---

## 5. インターフェース設計 (IPC API)

### 5.1. Commands (Frontend -> Backend)

| コマンド名 | 引数 | 説明 |
| --- | --- | --- |
| `open_port` | `port_name`, `config: SerialConfig` | 指定設定でポートを開く |
| `close_port` | なし | ポートを閉じる |
| `write_data` | `data: Vec<u8>` | データを送信する |
| `write_dtr` | `level: bool` | DTR信号を設定する |
| `write_rts` | `level: bool` | RTS信号を設定する |
| `get_read_data` | `offset: u64`, `length: u32` | 指定範囲のバイナリデータを取得 |
| `get_display_rows` | `start_row: u64`, `row_count: u32` | Hex表示用行データを取得 |
| `get_ascii_lines` | `start_line`, `line_count`, `show_ctrl`, `show_timestamp` | ASCII表示用行データを取得 |
| `list_ports` | なし | 利用可能なシリアルポート一覧を取得 |
| `export_log` | `path: String` | 受信データをファイルにエクスポート |
| `clear_data` | なし | 受信データをクリア |
| `get_clipboard_text` | `mode: String` | クリップボード用テキストを取得（hex/ascii） |

### 5.2. Events (Backend -> Frontend)

| イベント名 | ペイロード | 説明 |
| --- | --- | --- |
| `serial-status` | `{ connected: bool, port: string }` | 接続状態の変化（`WM_DEVICECHANGE` によるホットプラグ検知含む） |
| `data-update` | `{ total_bytes: u64 }` | 新規データ受信通知。フロントエンドは `get_display_rows` で表示データを取得。 |
| `log-error` | `{ message: string }` | ディスクフルなどのエラー通知 |

> **Note:** `data-update` イベントはフレームレート（約60fps = 16ms間隔）に合わせて発火する。高頻度なデータ受信があってもUIへの通知はフレームレートで間引かれる。データ本体はイベントに含めず、`get_display_rows` APIで必要な範囲のみを取得する。

---

## 6. 実装ロードマップ

各フェーズで「動作確認（Verification）」を徹底し、手戻りを防ぎながら進める。

---

### Phase 1: 疎通確認とシリアルポート基盤構築 (Tracer Bullet) ✓ 完了

*   **目標:** `serialport` クレートを使用してCOMポートを開き、基本的なReadができることを確認する。
*   **ステータス:** **完了 (Completed)**
    *   クロスプラットフォーム対応 (`serialport` crate)
    *   COMポート列挙 / 開閉
    *   基本設定 (Baudrate等)
    *   Rust Backend - React Frontend 連携

### Phase 2: 高速受信コアとメモリ管理 (The Engine) ✓ 完了

*   **目標:** 12Mbpsの連続受信においてもデータ欠落が発生しない「リングバッファ/チャンクシステム」を完成させる。
*   **ステータス:** **完了 (Completed)**
    *   DataStore / Chunk 構造体
    *   Worker Thread (受信)
    *   Logger Thread (ディスク退避)
    *   ObjectPool (SegQueue)
    *   自動クリーンアップメカニズム

### Phase 3: ビューアUIと仮想スクロール (The Viewer) ✓ 完了

*   **目標:** 受信した大量のデータを、React側で遅延なく表示する。
*   **ステータス:** **完了 (Completed)**
    *   `get_read_data`, `get_display_rows` API
    *   `data-update` イベント (60fps)
    *   Virtual Scrolling (スケーリング対応)
    *   HexViewer (Offset, Hex, ASCII)

### Phase 4: 基本機能の統合 (Integration) ✓ 完了

*   **目標:** 実用的なシリアルモニタとしての体裁を整える。
*   **ステータス:** **完了 (Completed)**
    *   ポート一覧の自動更新
    *   安全な切断処理
    *   バイナリログのエクスポート
    *   設定変更 UI

### Phase 5: 送信機能の実装 (Sending Capability) ✓ 完了

*   **目標:** テキストボックスからのデータ送信機能を実装する。
*   **ステータス:** **完了 (Completed)**
    *   `write` API
    *   Send Panel UI
    *   送信履歴 / Enter送信オプション
    *   Loopback テスト済み

---

### Phase 6: フロントエンド刷新とUI調整 (UI Overhaul) ✓ 完了

*   **目標:** 全体的なUIを見直し、提供されたデザインモックアップに合わせてリファインする。

#### 実装状況

*   [x] **全体レイアウト** (Settings / Send / Receive)
*   [x] **Hex / ASCII モード切替**
*   [x] **Timestamp 表示** (ASCII モード)
*   [x] **Line Wrap** (ASCII モード)
*   [x] **Show Ctrl** (制御文字可視化)
*   [x] **Auto Scroll**
*   [x] **Copy / Save / Clear 機能**
*   [x] **バイトベーススクロール** (Hex/ASCII 間で一貫したスクロール位置)
*   [ ] **Search / Filter 機能** (UI実装済み、バックエンド検索ロジック未実装) - Phase 8で対応
*   [ ] **Plotter 連携** (Phase 7で対応)

#### 6-A. スクロール機能仕様

##### スクロールモード

| モード | 動作 |
|--------|------|
| **Auto-scroll ON** | スクロール位置を常に下端に固定。新規データ追加時も下端を維持 |
| **Auto-scroll OFF** | ユーザー操作のみ。表示バイト位置を維持 |

##### 定数 (viewerConstants.ts)

| 定数 | 値 | 備考 |
|------|-----|------|
| `ROW_HEIGHT` | 20px | 行の高さ |
| `BUFFER_ROWS` | 50 | 上下バッファ行数 |
| `MAX_SCROLL_HEIGHT` | 10,000,000px | スケーリング閾値 |
| `THROTTLE_MS` | 100ms | スクロール更新間隔 |
| `BYTES_PER_ROW` | 16 | Hex用 |

##### バイトオフセット変換 (scrollUtils.ts)

```typescript
byteOffset = (scrollTop / scrollHeight) * totalBytes
scrollTop = (byteOffset / totalBytes) * scrollHeight
```

##### モード切り替え時

| 切り替え | 動作 |
|---------|------|
| Hex ↔ ASCII | バイトオフセット維持 |
| Auto ON → OFF | その瞬間の最新バイト位置を記憶 |
| Auto OFF → ON | 次回データ受信時に下端へ |

##### 実装ファイル

| ファイル | 責務 |
|---------|------|
| `scrollUtils.ts` | バイトオフセット⇔scrollTop変換関数 |
| `useByteScroll.ts` | バイトベースのスクロール hook |
| `HexViewer.tsx` | Hex表示（16バイト/行） |
| `AsciiViewer.tsx` | ASCII表示（Text列を主スクロール） |

### Phase 7: シリアルプロッタ (Serial Plotter) 📅 予定

*   **目標:** Arduino IDEシリアルプロッタのようなリアルタイムグラフ描画機能に加え、Grafanaのステートタイムラインのような状態表示機能を実装する。

#### 7-0. シリアルプロッタ概要

##### コンセプト

本プロッタは以下の2つの可視化モードを提供する：

| モード | 用途 | 参考 |
|--------|------|------|
| **ラインチャート (Line Chart)** | センサー値、アナログ入力等の連続データをリアルタイムでプロット | Arduino IDE Serial Plotter |
| **ステートタイムライン (State Timeline)** | 離散的な状態変化（ON/OFF、エラーコード等）を時系列で可視化 | Grafana State Timeline Panel |

##### アーキテクチャ概要

```text
[受信バッファ] → [行パーサー] → [データパーサー] → [プロットデータストア]

---

## 8. テスト方針 (Testing Policy)

### 8.1. Rustバックエンド

#### 単体テスト
```bash
cd src-tauri
cargo test --lib
```

#### Linting & Formatting
```bash
cd src-tauri
cargo clippy --lib
cargo fmt -- --check
```

### 8.2. Frontend (TypeScript)

#### Type Checking & Linting
```bash
npm run type-check
npm run lint
```

### 8.3. 継続的インテグレーション (CI)

GitHub Actionsにより、Pull Request作成時とpush時に以下が自動実行されます：
- `cargo test --lib`
- `cargo clippy --lib`
- `cargo fmt -- --check`
- `npm run tauri build`

### 8.4. E2E / 実機テスト
Pythonスクリプトを用いた実機テスト（Raspberry Pi Pico）や仮想COMポートテストの手順は、[test_tools/README.md](../test_tools/README.md) を参照してください。


---

#### 7-1. データフォーマット仕様

##### 7-1-1. サポートするデータ形式

Arduino IDE互換の形式を基本とし、拡張形式もサポートする。

| 形式 | 説明 | 例 |
|------|------|----|
| **単一値** | 1行に1つの数値 + 改行 | `123.45\r\n` |
| **複数値 (CSV)** | カンマ/タブ/スペース区切り | `10,20,30\r\n` または `10\t20\t30\r\n` |
| **ラベル付き値** | `label:value` 形式 | `temp:25.5,humidity:60\r\n` |
| **ヘッダー行** | 列名定義（初回のみ） | `temp,humidity,pressure\r\n` |
| **状態値** | 文字列/数値で状態を表現 | `state:RUNNING\r\n` または `motor:1\r\n` |

##### 7-1-2. 改行コード

以下の改行コードを自動認識する：

- `\r\n` (CRLF) - Windows / Arduino標準
- `\n` (LF) - Unix/Linux
- `\r` (CR) - 旧Mac

##### 7-1-3. データパース仕様

```rust
/// パースされた1行分のデータ
#[derive(Debug, Clone)]
pub struct ParsedDataPoint {
    /// 受信時刻（ミリ秒精度）
    pub timestamp_ms: u64,
    /// チャンネルデータ（ラベル → 値）
    pub channels: HashMap<String, ChannelValue>,
}

/// チャンネル値（数値または状態）
#[derive(Debug, Clone)]
pub enum ChannelValue {
    /// 数値 (Line Chart用)
    Numeric(f64),
    /// 状態値 (State Timeline用)
    State(String),
}
```

##### 7-1-4. パースルール

1. **行区切り:** 改行コードで1データポイントを区切る
2. **値区切り:** カンマ `,` > タブ `\t` > スペース ` ` の優先順位で検出
3. **ラベル検出:** コロン `:` でラベルと値を分離
4. **型推論:**
   - 数値としてパース可能 → `ChannelValue::Numeric`
   - パース不可 → `ChannelValue::State`
5. **ラベル自動生成:** ラベルがない場合は `ch0`, `ch1`, ... を自動付与
6. **ヘッダー検出:** 最初の行がすべて非数値の場合、ヘッダーとして解釈

##### 7-1-5. エラーハンドリング

| ケース | 挙動 |
|--------|------|
| パース失敗行 | 該当行をスキップしてログに警告を出力 |
| 値の欠落 | 直前の値を継続（ホールド） |
| 異常な数値 (NaN, Inf) | 除外してプロットに含めない |

---

#### 7-2. ラインチャート (Line Chart)

##### 7-2-1. 基本機能

| 機能 | 説明 |
|------|------|
| **リアルタイム描画** | 受信データをリアルタイムでプロット（最大60fps） |
| **複数チャンネル** | 最大16チャンネルを同時表示 |
| **カラー自動割り当て** | 各チャンネルに異なる色を自動割り当て |
| **凡例 (Legend)** | チャンネル名と現在値をツールチップ/パネルで表示 |
| **時間軸スクロール** | X軸は時間を表し、新データで自動スクロール |

##### 7-2-2. 軸設定

| 項目 | 説明 | デフォルト |
|------|------|-----------|
| **X軸 (時間軸)** | 表示幅を秒単位で設定 | 10秒 |
| **Y軸 (値軸)** | Auto-scale / 手動レンジ設定 | Auto |
| **Y軸最小/最大** | 手動レンジ時の固定値 | - |

##### 7-2-3. 表示データ量

| 設定 | 説明 | デフォルト |
|------|------|-----------|
| **表示ポイント数** | X軸上の最大ポイント数 | 500 |
| **スクロールウィンドウ** | 古いデータを押し出すスクロール幅 | 表示幅 |

##### 7-2-4. ズーム・パン

| 操作 | 動作 |
|------|------|
| **マウスホイール** | Y軸ズーム |
| **Shift + ホイール** | X軸ズーム |
| **ドラッグ** | パン（過去データ閲覧） |
| **ダブルクリック** | Auto-scale復帰 |

##### 7-2-5. 間引き戦略 (Downsampling)

高速データ受信時の描画負荷軽減：

| 手法 | 説明 |
|------|------|
| **LTTB (Largest Triangle Three Buckets)** | 波形の特徴を維持したダウンサンプリング |
| **Min-Max** | 各バケット内の最小・最大を描画 |
| **平均値** | 各バケットの平均を描画 |

##### 7-2-6. UI レイアウト

```
┌─────────────────────────────────────────────────────────────────────┐
│ [▶ Start] [⏸ Pause] [⏹ Stop] [📁 Export]    X: 10s ▼  Y: Auto ▼     │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│                      [ Line Chart Area ]                            │
│                                                                     │
│     ^ 100                                                          │
│     │    ╱╲    ╱╲                                                   │
│     │   ╱  ╲  ╱  ╲                                                  │
│     │  ╱    ╲╱    ╲                                                 │
│     │ ╱              ╲                                              │
│     └────────────────────────────────────────────────────────────▶  │
│       0s                                                   10s      │
├─────────────────────────────────────────────────────────────────────┤
│ 凡例: ● ch0: 45.2  ● ch1: 23.1  ● ch2: 67.8                         │
└─────────────────────────────────────────────────────────────────────┘
```

---

#### 7-3. ステートタイムライン (State Timeline)

##### 7-3-1. 基本機能

| 機能 | 説明 |
|------|------|
| **状態可視化** | 離散的な状態変化を水平バーで表示 |
| **複数チャンネル** | 各状態チャンネルを行として表示 |
| **色分け** | 状態ごとに異なる色を割り当て |
| **持続時間表示** | 各状態の継続時間をバーの長さで表現 |

##### 7-3-2. 状態の解釈

| データ型 | 状態解釈 |
|----------|----------|
| **文字列** | そのまま状態名として使用 |
| **数値（離散）** | 閾値設定で状態に変換 (例: 0=OFF, 1=ON) |
| **Boolean** | true/false を ON/OFF として表示 |

##### 7-3-3. 表示設定

| 項目 | 説明 | デフォルト |
|------|------|-----------|
| **行高さ** | 各チャンネルの行の高さ | 24px |
| **値表示** | バー内に状態名を表示 | ON |
| **色マッピング** | 状態 → 色 の対応設定 | 自動 |

##### 7-3-4. カスタム色マッピング

```json
{
  "stateColors": {
    "RUNNING": "#22c55e",
    "STOPPED": "#ef4444",
    "IDLE": "#f59e0b",
    "ERROR": "#dc2626"
  }
}
```

##### 7-3-5. 閾値ベース状態変換

数値データを状態に変換するための閾値設定：

```json
{
  "thresholds": [
    { "value": 0, "state": "OFF", "color": "#6b7280" },
    { "value": 1, "state": "ON", "color": "#22c55e" },
    { "value": 2, "state": "WARNING", "color": "#f59e0b" },
    { "value": 3, "state": "ERROR", "color": "#ef4444" }
  ]
}
```

##### 7-3-6. UI レイアウト

```
┌─────────────────────────────────────────────────────────────────────┐
│ [▶ Start] [⏸ Pause] [⏹ Stop]                    Time: 30s ▼        │
├─────────────────────────────────────────────────────────────────────┤
│  0s              10s              20s              30s              │
├─────────────────────────────────────────────────────────────────────┤
│ motor:   ████ ON ████│░░ OFF ░░│███████ ON ███████│░ OFF │         │
├─────────────────────────────────────────────────────────────────────┤
│ pump:    ░░░ OFF ░░░░│████ ON ████████████████████│░ OFF │         │
├─────────────────────────────────────────────────────────────────────┤
│ valve:   ████ OPEN ██│░░ CLOSED │██ OPEN █│░ CLOSED ░░░░│         │
├─────────────────────────────────────────────────────────────────────┤
│ error:   ─────────────────────────│██ ERR │─────────────│         │
└─────────────────────────────────────────────────────────────────────┘
```

---

#### 7-4. プロッタウィンドウ設計

##### 7-4-1. ウィンドウ構成

プロッタは**メインウィンドウとは独立した別ウィンドウ**として開く。

| 項目 | 仕様 |
|------|------|
| **起動方法** | メインウィンドウの「📈 Plotter」ボタン |
| **ウィンドウサイズ** | 800 x 600 (可変) |
| **複数ウィンドウ** | 複数のプロッタウィンドウを開くことが可能 |
| **データ連携** | メインウィンドウと同一のシリアルセッションを参照 |

##### 7-4-2. 表示モード切替

| モード | 説明 |
|--------|------|
| **Line Chart Only** | ラインチャートのみ表示 |
| **State Timeline Only** | ステートタイムラインのみ表示 |
| **Split View** | 上下または左右に両方を表示 |

##### 7-4-3. データソース設定

| 項目 | 説明 |
|------|------|
| **チャンネル選択** | 表示するチャンネルを選択（チェックボックス） |
| **Line/State振り分け** | 各チャンネルをLine ChartかState Timelineに振り分け |
| **自動検出** | データ型から自動的に振り分け（推奨） |

---

#### 7-5. バックエンド実装

##### 7-5-1. データストア設計

```rust
/// プロッタ用のデータストア
pub struct PlotterDataStore {
    /// チャンネル定義（ラベル → インデックス）
    channels: HashMap<String, usize>,
    
    /// ライン用データバッファ（リングバッファ）
    line_data: Vec<RingBuffer<(u64, f64)>>,  // (timestamp_ms, value)
    
    /// ステート用データ（状態変化リスト）
    state_data: Vec<Vec<StateChange>>,
    
    /// 設定
    config: PlotterConfig,
}

/// 状態変化
#[derive(Debug, Clone)]
pub struct StateChange {
    pub start_ms: u64,
    pub end_ms: Option<u64>,  // O ongoing
    pub state: String,
}

/// プロッタ設定
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlotterConfig {
    /// 保持するポイント数
    pub max_points: usize,  // デフォルト: 10000
    
    /// チャンネルタイプ設定
    pub channel_types: HashMap<String, ChannelType>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChannelType {
    Line,
    State,
    Auto,
}
```

##### 7-5-2. Tauri Commands

| コマンド | 引数 | 説明 |
|----------|------|------|
| `get_plotter_data` | `start_ms`, `end_ms`, `channels` | 指定範囲のプロットデータを取得 |
| `get_plotter_channels` | - | 利用可能なチャンネル一覧を取得 |
| `set_plotter_config` | `config` | プロッタ設定を更新 |
| `export_plotter_data` | `path`, `format` | データをCSV/JSONでエクスポート |

##### 7-5-3. Tauri Events

| イベント | ペイロード | 説明 |
|----------|-----------|------|
| `plotter-update` | `{ channels: [...], timestamp: u64 }` | 新規データ通知（60fps間引き） |
| `plotter-channel-added` | `{ name: string, type: string }` | 新チャンネル検出通知 |

---

#### 7-6. フロントエンド実装

##### 7-6-1. 技術選定

| 要素 | 選定 | 理由 |
|------|------|------|
| **ラインチャート** | [uPlot](https://github.com/leeoniya/uPlot) | 軽量・高速・リアルタイム向け |
| **ステートタイムライン** | カスタム実装 (Canvas/SVG) | 特化したUI要件のため |
| **状態管理** | React + useState/useEffect | シンプルな状態管理 |

##### 7-6-2. コンポーネント構成

```
src/
├── plotter/
│   ├── PlotterWindow.tsx       # メインウィンドウ
│   ├── LineChart.tsx           # uPlotラッパー
│   ├── StateTimeline.tsx       # ステートタイムライン
│   ├── ChannelSelector.tsx     # チャンネル選択UI
│   ├── PlotterControls.tsx     # 再生/停止/設定
│   └── hooks/
│       ├── usePlotterData.ts   # データ取得フック
│       └── usePlotterConfig.ts # 設定管理フック
```

##### 7-6-3. パフォーマンス最適化

| 最適化 | 説明 |
|--------|------|
| **Web Workers** | データパースをワーカースレッドで実行 |
| **canvas描画** | DOM更新を最小化しcanvasで直接描画 |
| **requestAnimationFrame** | 描画を60fpsにスロットリング |
| **TypedArray** | データ格納にFloat64Arrayを使用 |

---

#### 7-7. 実装ステップ

##### 7-7-1. 数値パースロジック (Backend)
*   **作業内容:** 受信ストリームから CSV/ラベル付きデータをパースするロジック実装。
*   **中間確認:**
    *   [ ] 単一値がパースできる
    *   [ ] CSV形式（カンマ区切り）がパースできる
    *   [ ] タブ/スペース区切りがパースできる
    *   [ ] ラベル付き形式 (`label:value`) がパースできる
    *   [ ] ヘッダー行を検出・解釈できる
    *   [ ] パースエラー時にクラッシュせずスキップする

##### 7-7-2. プロッタデータストア (Backend)
*   **作業内容:** パース済みデータを格納するリングバッファ実装。
*   **中間確認:**
    *   [ ] ポイントがリングバッファに格納される
    *   [ ] 古いデータが自動的に削除される
    *   [ ] チャンネルが動的に追加される
    *   [ ] 状態変化が正しく記録される

##### 7-7-3. Tauri Command/Event実装 (Backend)
*   **作業内容:** フロントエンドとの通信APIを実装。
*   **中間確認:**
    *   [ ] `get_plotter_data` で指定範囲のデータが取得できる
    *   [ ] `plotter-update` イベントが60fpsで発火する
    *   [ ] 新チャンネル検出時に通知される

##### 7-7-4. ラインチャート実装 (Frontend)
*   **作業内容:** uPlotを使用したリアルタイムグラフ描画。
*   **中間確認:**
    *   [ ] uPlotがレンダリングされる
    *   [ ] リアルタイムでデータが追加される
    *   [ ] Auto-scaleが機能する
    *   [ ] 複数チャンネルが色分けで表示される
    *   [ ] ズーム/パンが動作する

##### 7-7-5. ステートタイムライン実装 (Frontend)
*   **作業内容:** カスタムコンポーネントで状態変化を可視化。
*   **中間確認:**
    *   [ ] 状態がカラーバーで表示される
    *   [ ] 複数チャンネルが行として表示される
    *   [ ] 状態名がバー内に表示される
    *   [ ] 時間軸が同期している

##### 7-7-6. プロッタウィンドウ統合 (Frontend)
*   **作業内容:** 別ウィンドウとしてプロッタを起動する仕組み。
*   **中間確認:**
    *   [ ] 「📈 Plotter」ボタンで新ウィンドウが開く
    *   [ ] メインウィンドウと同期してデータが表示される
    *   [ ] ウィンドウを閉じても再度開ける
    *   [ ] 複数ウィンドウが開ける

##### 7-7-7. 設定UI実装 (Frontend)
*   **作業内容:** チャンネル選択、表示設定等のUI。
*   **中間確認:**
    *   [ ] チャンネル選択UIが表示される
    *   [ ] Line/Stateの振り分けを変更できる
    *   [ ] 表示時間幅を変更できる
    *   [ ] Y軸レンジを変更できる

##### 7-7-8. パフォーマンスチューニング
*   **作業内容:** 高速受信時 (12Mbps) の描画負荷対策。
*   **中間確認:**
    *   [ ] 12Mbpsデータ受信中でもUIがフリーズしない
    *   [ ] 描画がデータの流れに追従する
    *   [ ] メモリ使用量が一定範囲で安定する

---

#### Phase 7 完了条件 (Verification)

##### 機能テスト
*   [ ] **CSVデータ表示:** Arduino形式のCSVデータがラインチャートに正しくプロットされる
*   [ ] **ラベル付きデータ:** `temp:25.5,humidity:60` 形式がパースされ、チャンネル名で凡例表示される
*   [ ] **状態表示:** `state:RUNNING` 形式がステートタイムラインに正しく表示される
*   [ ] **自動チャンネル検出:** データ型に応じてLine/Stateが自動判別される

##### パフォーマンステスト
*   [ ] **リアルタイム追従:** 1kHz (1000サンプル/秒) のデータが遅延なく表示される
*   [ ] **高速データ耐性:** 12Mbps受信中でもプロッタがクラッシュしない
*   [ ] **メモリ安定性:** 長時間稼働でもメモリ使用量が増え続けない

##### ユーザビリティテスト
*   [ ] **ズーム/パン:** 過去データの確認ができる
*   [ ] **エクスポート:** CSVでデータをエクスポートできる
*   [ ] **設定保持:** ウィンドウを閉じて再度開いても設定が維持される

