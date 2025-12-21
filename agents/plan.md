# SerialMonitorEssential 設計仕様書

## 1. プロジェクト概要

FPGAや高速センサ開発に耐えうる、**12Mbps級の高速シリアル通信**に対応したデスクトップアプリケーション。
RustによるWin32 APIの直接制御で「データの完全性（取りこぼしゼロ）」を保証しつつ、Tauri (React) を用いて「モダンで応答性の高いUI」を提供する。

## 2. 要求仕様 (Requirements)

### 2.1. 通信・制御 (Backend)

* **高速通信:** 12Mbps などの非標準・高速ボーレートに対応（Win32 API許容最大値まで）。
* **データ完全性:** 受信データは内部で1バイトも取りこぼさず保持・ログ保存する。UI描画遅延によるバッファ溢れを防ぐ。
* **ホットプラグ:** `WM_DEVICECHANGE` メッセージを用いてUSBケーブルの抜き差しを検知し、適切に切断・再接続待機状態へ遷移する。
* **複数起動:** アプリの複数起動を許可し、それぞれで別のCOMポートを監視可能にする。
* **受信エラー処理:** フレーミングエラー、パリティエラー等の受信エラーは無視してデータ受信を継続する。

### 2.2. データ管理・ログ (Data & Logging)

* **全データ保持:** 受信開始から終了までの全データを閲覧可能にする（メモリが許す限りではなく、ディスクを活用して制限を撤廃）。
* **隠蔽されたキャッシュ:** 一時データは `%TEMP%\SerialMonitorEssential\<PID>` 領域のファイルに逃がし、アプリ終了時に自動削除する（ユーザーのディスクを汚さない）。PIDを用いることで複数インスタンス間のファイル名衝突を回避する。
* **ログ保存:**
  * ユーザー操作による任意のタイミングでの保存開始/停止。
  * 指定時間（例: 10分）ごとのファイル分割（ローテーション）機能。ファイル名はタイムスタンプ形式（例: `log_20231219_143052`）とする。
  * 保存形式はバイナリ/テキストを選択可能。



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
| `open_port` | `port_name`, `baud_rate`, `config` | 指定設定でポートを開く |
| `close_port` | なし | ポートを閉じる |
| `write_data` | `bytes: Vec<u8>` | データを送信する |
| `start_logging` | `file_path`, `rotation_minutes` | ログ保存を開始 |
| `stop_logging` | なし | ログ保存を停止 |
| `get_read_data` | `offset: u64`, `length: u32` | 指定範囲のバイナリデータを取得 |
| `get_display_rows` | `start_row: u64`, `row_count: u32` | 表示用行データを取得（Hex/ASCII変換済み） |

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

### 6.0. 段階的検証方針 (Incremental Verification Policy)

本プロジェクトでは、**各実装ステップごとに細かく動作確認を行い、問題を早期に発見・修正する**方針を採る。

#### 基本原則

1.  **「1ステップ = 1検証」の法則:**
    *   コードを書いたら即座にビルド・実行して動作を確認する。大きな変更を一気に行わない。
    *   確認できたらコミットし、次のステップに進む。

2.  **最小単位での動作確認:**
    *   新しい関数やモジュールは、単体テストまたは `println!` デバッグで個別に確認してから統合する。
    *   バックエンドの新規API追加時は、まずRust側の単体テストで動作を確認し、その後Tauri Commandとして公開してフロントエンドから呼び出す。

3.  **確認項目の記録:**
    *   各ステップの確認結果（OK/NG）は、開発ログまたはコミットメッセージに記載する。
    *   NGの場合は原因と対処を記録し、同じ問題の再発を防ぐ。

4.  **段階的統合 (Incremental Integration):**
    *   複数コンポーネントの結合は、1つずつ行う。
    *   例: スレッドA単体で動作確認 → スレッドB単体で動作確認 → A+Bの結合確認

#### 確認手法の選択基準

| 確認対象 | 推奨手法 |
| --- | --- |
| 純粋なロジック（計算、変換） | 単体テスト (`#[test]`) |
| Win32 API コール | `println!` / ログ出力 + 手動確認 |
| スレッド間通信 | ログ出力 + 耐久テスト |
| IPC (Tauri Command) | フロントエンドからの呼び出し + DevTools Console |
| UI表示 | 目視確認 + スクリーンショット記録 |

---

### Phase 1: 疎通確認とWin32 API基盤構築 (Tracer Bullet)

*   **目標:** Win32 APIを直接叩いてCOMポートを開き、12Mbpsの設定投入と、基本的なReadができることを確認する。UIへの表示はコンソールまたは最低限のTauri画面で行う。

#### 1-1. Rustプロジェクトセットアップ
*   **作業内容:** `windows` クレートの導入、Tauriプロジェクトの初期化。
*   **中間確認:**
    *   [x] `cargo build` が成功する
    *   [x] `cargo tauri dev` でウィンドウが表示される

#### 1-2. COMポート列挙
*   **作業内容:** 利用可能なCOMポート一覧を取得する機能の実装。
*   **中間確認:**
    *   [x] Rust側で `SetupDiGetClassDevs` 等を用いてポート一覧を取得できる
    *   [x] Tauri Command `list_ports` でフロントエンドからポート名一覧を取得できる

#### 1-3. SerialPort構造体（Create/Close）
*   **作業内容:** `CreateFileW` でポートを開き、`CloseHandle` で閉じるラッパー実装。
*   **中間確認:**
    *   [x] 存在するCOMポートを `CreateFileW` で開ける（ハンドル取得成功）
    *   [x] 存在しないポートを開こうとすると適切なエラーが返る
    *   [x] `CloseHandle` 後に再度開けることを確認（ハンドルリーク確認）

#### 1-4. SerialPort構造体（DCB設定）
*   **作業内容:** `SetCommState` (DCB設定), `SetupComm`, `SetCommTimeouts` のラッパー実装。
*   **中間確認:**
    *   [x] 9600bps, 8N1 の標準設定が投入できる
    *   [x] 12Mbps 設定投入時にエラーが返らない（対応ドライバがある場合）
    *   [x] 不正なボーレート（0や負値）でエラーハンドリングできる

#### 1-5. Basic Reader
*   **作業内容:** `ReadFile` を用いた単純な受信ループの実装。
*   **中間確認:**
    *   [x] ループバックテスト: TX-RX短絡で送信データがそのまま受信できる
    *   [x] タイムアウト設定が機能し、データがない時にCPU 100%にならない
    *   [x] 受信データを `println!` で表示できる

#### 1-6. Tauri Command統合
*   **作業内容:** フロントエンドから `open_port` / `close_port` / `write_data` を呼べるように繋ぐ。
*   **中間確認:**
    *   [x] React UIのボタンからポートを開ける
    *   [x] 開いた状態で閉じるボタンが機能する
    *   [x] エラー時にフロントエンドにエラーメッセージが返る

#### Phase 1 完了条件 (Verification)
*   [x] **ループバックテスト:** USBシリアル変換器のTX-RXを短絡し、送信した文字列がそのまま受信できるか確認 (115200bps等でOK)。
*   [x] **設定投入確認:** 12Mbps等の高速ボーレート設定時にエラーが返らないか確認。
*   [x] **CPU負荷確認:** 単純ループでCPUを100%食いつぶしていないこと。

---

### Phase 2: 高速受信コアとメモリ管理 (The Engine)

*   **目標:** 12Mbpsの連続受信においてもデータ欠落が発生しない「リングバッファ/チャンクシステム」を完成させる。UI表示はまだ行わない。

#### 2-1. Chunk構造体
*   **作業内容:** `Chunk` データ構造の定義と基本操作の実装。
*   **中間確認:**
    *   [x] `Chunk` の生成・破棄が正しく動作する
    *   [x] `valid_len` の更新が正しく行われる
    *   [x] 単体テストで境界値（0バイト、満杯、オーバーフロー試行）を確認

#### 2-2. ObjectPool (SegQueue)
*   **作業内容:** `ObjectPool` の実装（空きチャンクの管理）。
*   **中間確認:**
    *   [x] `get_free_chunk()` で空きChunkが取得できる
    *   [x] `return_chunk()` で返却したChunkが再取得できる
    *   [x] プールが空の時の挙動（ブロック or 新規確保）が意図通り

#### 2-3. Worker Thread（受信スレッド）
*   **作業内容:** 受信スレッドの実装。`ReadFile` して `Chunk` に詰め、満杯/タイムアウトで `FinishedQueue` に流す。
*   **中間確認:**
    *   [x] 別スレッドで受信ループが動作する
    *   [x] 16ms タイムアウトでチャンクがスワップされる
    *   [x] 満杯時にもスワップされる
    *   [x] `println!` でチャンク完了ログを確認

#### 2-4. FinishedQueue
*   **作業内容:** 受信完了チャンクのキュー実装。
*   **中間確認:**
    *   [x] Worker → Queue への push が成功する
    *   [x] Queue からの pop が成功する
    *   [x] 複数スレッドからの同時アクセスで競合しない

#### 2-5. Logger Thread（保存スレッド）
*   **作業内容:** Queueから取り出して一時ファイルへ書き出す。Arc による自動メモリ管理。
*   **中間確認:**
    *   [x] `%TEMP%\SerialMonitorEssential\<PID>` にファイルが作成される
    *   [x] 書き込まれたデータが正しい（バイナリエディタで確認）
    *   [x] 書き込み後に Arc 参照カウントが減少しメモリが解放される

#### 2-6. PageMetadata管理
*   **作業内容:** ディスク書き出し済みデータのインデックス管理。
*   **中間確認:**
    *   [x] `PageMetadata` が正しく追加・検索できる
    *   [x] `global_offset` から該当ファイルとオフセットを特定できる

#### 2-7. Drop & Cleanup
*   **作業内容**: 起動時に古い一時ファイルをクリーンアップする仕組み。
*   **設計変更**: Drop trait だけに頼らず、起動時にプロセス存在確認で古いフォルダを削除する方式に変更。
*   **中間確認:**
    *   [x] 起動時に古いPIDフォルダが削除される
    *   [x] プロセス名確認で SerialMonitorEssential のプロセスだけを保護
    *   [x] 複数インスタンス起動時に互いのフォルダを削除しない
    *   [x] Drop trait でもベストエフォートでクリーンアップを試みる

#### Phase 2 完了条件 (Verification)
*   [x] **ビルド成功:** `cargo build` が成功する
*   [x] **単体テスト:** `cargo test --lib` が成功する
*   [x] **起動時クリーンアップ:** 前回実行の一時ファイルが次回起動時に削除される
*   [x] **複数インスタンス安全性:** 複数起動時に互いのフォルダを削除しない
*   [x] **データ受信動作:** データが data.bin に正しく書き込まれる
*   [x] **高負荷耐久テスト:** 12Mbpsでダミーデータを1分間流し続け、受信バイト総数が送信バイト数と **1バイトの狂いもなく** 一致することを確認する（実機テスト必要）。
*   [x] **メモリリーク確認:** メモリ使用量が一定範囲で頭打ちになり、増え続けないことを確認する（実機テスト必要）。

> [!IMPORTANT]
> **Phase 2完了時の必須データ構造チェックリスト:**
> 
> Phase 3へ進む前に、以下の項目を **必ず確認** してください：
> 
> - [x] `Chunk` 構造体に `global_offset: u64` フィールドが存在する
> - [x] `Chunk` に `set_global_offset(&mut self, offset: u64)` メソッドが実装されている
> - [x] `Chunk` に `global_offset(&self) -> u64` ゲッターが実装されている
> - [x] `DataStore` の finished_queue が `Arc<RwLock<VecDeque<Arc<Chunk>>>>` として実装されている
> - [x] Worker Thread がチャンクに `global_offset` を設定してから finished_list へ追加している
> - [x] Worker Thread が `finished_list.write().push_back(Arc::new(chunk))` を使用している
> - [x] Logger Thread が `finished_list.read().front()` で参照後、`finished_list.write().pop_front()` で削除している
> - [x] Logger Thread が Arc<Chunk> を扱い、free_poolへの手動返却を行っていない（Arcのdropでメモリ解放）
> - [x] `get_data` メソッドが `archived_index`（確定データ）を先に検索し、`finished_list`（最新データ）にフォールバックする
> - [x] `total_bytes` メソッドが finished_list の最新チャンクも考慮している


---

### Phase 3: ビューアUIと仮想スクロール (The Viewer) ✓ 完了

*   **目標:** 受信した大量のデータを、React側で遅延なく表示する。

#### 3-1. get_read_data API（Backend） ✓
*   **作業内容:** `get_read_data(offset, length)` の実装。メモリまたはディスクから該当データをフェッチして返す。
*   **中間確認:**
    *   [x] メモリ上（FinishedQueue内）のデータを取得できる
    *   [x] ディスク上（archived）のデータを取得できる
    *   [x] 境界をまたぐ読み出し（メモリ+ディスク）が正しく動作する
    *   [x] 無効なoffset/lengthでエラーが返る

#### 3-2. data-update イベント ✓
*   **作業内容:** バックエンドからフロントエンドへの新規データ通知イベント実装。
*   **中間確認:**
    *   [x] データ受信時に `data-update` イベントが発火する
    *   [x] 60fps (16ms) 間隔で間引きされている
    *   [x] フロントエンドでイベントを受信し、`total_bytes` が更新される

#### 3-3. Virtual Scrolling コンポーネント ✓
*   **作業内容:** カスタムスケーリング仮想スクロールを実装（ブラウザDOM制限対応）。
*   **中間確認:**
    *   [x] スクロールコンテナが表示される
    *   [x] 2,024,256行（32MB）でスクロールが滑らかに動作する
    *   [x] 見える範囲の行だけがDOMにレンダリングされている

#### 3-4. Hex/ASCII表示パーサー ✓
*   **作業内容:** バイナリデータをHex/ASCII表示用に加工するロジック実装（Rust側）。
*   **中間確認:**
    *   [x] バイト配列を16進数文字列に変換できる
    *   [x] 制御文字 (CR, LF, NULL等) が視覚的に識別できる形で表示される

#### 3-5. Backend-Frontend統合 ✓
*   **作業内容:** `get_display_rows` APIでバックエンド駆動のデータ取得を実装。
*   **中間確認:**
    *   [x] スクロール位置に応じたoffsetでデータを取得できる
    *   [x] 取得したデータが正しく表示される
    *   [x] 受信中にリアルタイムで表示が更新される

#### Phase 3 完了条件 (Verification) ✓
*   [x] **スクロール性能:** 32MB以上のデータで60fps維持
*   [x] **データ整合性:** 受信データのSHA256チェックサム一致 (100%)
*   [x] **表示遅延:** 受信中のデータが画面に反映されるまでのラグが体感0.1秒以下

---

### Phase 4: 基本機能の統合 (Integration)

*   **目標:** 実用的なシリアルモニタとしての体裁を整える。

#### 4-1. WM_DEVICECHANGE 検知
*   **作業内容:** デバイスの挿抜を検知するメッセージハンドラ実装。
*   **中間確認:**
    *   [ ] USBケーブル挿入時にログ出力される
    *   [ ] USBケーブル抜去時にログ出力される

#### 4-2. 安全な切断処理
*   **作業内容:** デバイス切断時に安全にポートを閉じ、UIに通知する。
*   **中間確認:**
    *   [ ] 受信中に切断してもアプリがクラッシュしない
    *   [ ] 切断イベントがフロントエンドに通知される
    *   [ ] 受信スレッドが正しく終了する

#### 4-3. ポート一覧の動的更新
*   **作業内容:** 挿抜に応じてポート一覧UIを自動更新。
*   **中間確認:**
    *   [ ] デバイス挿入時にポート一覧に追加される
    *   [ ] デバイス抜去時にポート一覧から削除される

#### 4-4. ログエクスポート機能
*   **作業内容:** 現在の受信データをユーザー指定のパスへ保存する機能。
*   **中間確認:**
    *   [ ] ファイル保存ダイアログが表示される
    *   [ ] 指定パスにファイルが保存される
    *   [ ] 保存されたファイルの内容が正しい（バイナリエディタで確認）

#### 4-5. ログローテーション
*   **作業内容:** 指定時間ごとにログファイルを分割する機能。
*   **中間確認:**
    *   [ ] 設定した時間（例: 10分）でファイルが分割される
    *   [ ] ファイル名にタイムスタンプが含まれる
    *   [ ] 連続するファイルを結合すると元データと一致する

#### 4-6. 設定画面UI
*   **作業内容:** ボーレート、ポート番号、データビット等の設定画面。
*   **中間確認:**
    *   [ ] 設定項目が表示される
    *   [ ] 設定変更がバックエンドに反映される
    *   [ ] 再接続時に変更した設定が適用される

#### Phase 4 完了条件 (Verification)
*   [ ] **切断耐性:** 受信中にUSBケーブルを物理的に引き抜き、アプリがクラッシュせず、エラー表示に切り替わること。
*   [ ] **再接続:** 再度ケーブルを挿した際、手動で再接続できること。
*   [ ] **ログ検証:** エクスポートしたログファイルをバイナリエディタで開き、破損がないか確認。

---

### Phase 5: 高度な機能 (Advanced Features)

*   **目標:** グラフ表示や送信機能などの付加価値。

#### 5-1. 送信バッファ実装
*   **作業内容:** 送信データのバッファリングと `WriteFile` による送信。
*   **中間確認:**
    *   [ ] `WriteFile` でデータ送信が成功する
    *   [ ] 大きなデータも分割して送信できる

#### 5-2. 送信UI
*   **作業内容:** テキスト/バイナリ入力、Enterキー挙動設定、送信履歴。
*   **中間確認:**
    *   [ ] テキスト入力欄から送信できる
    *   [ ] Hex入力モードが動作する
    *   [ ] Up Arrowで送信履歴を呼び出せる

#### 5-3. リアルタイムグラフ描画
*   **作業内容:** 受信データから数値をパースし、`uPlot` でリアルタイムグラフを描画。
*   **中間確認:**
    *   [ ] 数値データをパースできる
    *   [ ] グラフが表示される
    *   [ ] リアルタイムでグラフが更新される
    *   [ ] 高速データ時に間引き処理が機能する

#### Phase 5 完了条件 (Verification)
*   [ ] **送信確認:** ループバックで送信データが即座に受信側に表示されるか。
*   [ ] **グラフ追従性:** 高速データ更新時にグラフ描画が追いつくか（間引き処理の調整）。

