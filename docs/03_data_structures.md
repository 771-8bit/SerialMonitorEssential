# データ構造とメモリ管理 (Chunk-based Circular Buffering)

メモリ割り当て（malloc/free）のオーバーヘッドを排除し、GCのないRustの利点を活かした「チャンク循環システム」を採用する。

## データ構造

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

## 読み書きフロー (The Bucket Relay)

### 1. 初期化

*   起動時に `Chunk` を一定数（例: 100個 = 約6.4MB）生成し、`free_pool` に投入。
*   プール枯渇時は新規 `Chunk` を作成（Logger 追従中は稀）。

### 2. Write (受信フェーズ) - Worker Thread

*   `free_pool` から空チャンクを1つ取り出す（なければ新規作成）。
*   シリアルデータを書き込む。
*   **16ms経過** または **満杯** になったら `Arc::new(chunk)` で `finished_list` の末尾へ追加（Publish）。
*   **空データは追加しない**（16ms経ってもデータ0バイトならスワップしない）。

### 3. Persist (保存フェーズ) - Logger Thread

*   `finished_list` の **先頭** を `front()` で参照（まだ削除しない）。
*   ディスクへ追記書き込み。
*   書き込み完了後、`archived_index` を更新。
*   **最後に** `pop_front()` でチャンクを削除（Arc 参照カウント減少 → 0 でメモリ解放）。

### 4. Read (UI/Viewer) - UI Thread via API

*   **リアルタイム表示:** `data-update` イベントで `total_bytes` を受信 → `get_display_rows` APIで表示データを取得。
*   **過去データ表示:** スクロール位置に応じて `get_display_rows` APIでバックエンドからデータ取得。
*   **バックエンド処理:** Hex/ASCII変換はRust側で実行、フロントエンドは整形済みデータを受け取る。
*   **データ検索順序:** `get_data` は `archived_index`（確定データ）を先に検索し、足りない部分を `finished_list`（最新データ）から取得。これにより境界をまたぐリクエストにも正しく対応。

## UIへのデータ通知 (Push Model)

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

## バックエンド駆動データ表示 (Backend-Driven Pagination)

**問題点:** 仮想スクロールライブラリ（virtua等）は表示範囲を絞るが、全行の配列をフロントエンドで生成する必要があり、大量データ（32MB = 200万行）でOOMが発生する。

**解決策:** **バックエンド（Rust）で表示範囲のデータを絞り込み、必要な行データのみをフロントエンドに送信する。**

### データフローモデル

```
[Frontend]                              [Backend (Rust)]
     |                                        |
     |-- open_port -------------------------->|
     |                                        |
     |<-- data-update { total_bytes } --------|  (60fps間引き)
     |                                        |
     |-- get_display_rows(start_row, count) ->|  (スクロール時)
     |                                        |
     |<-- DisplayRowsPayload { rows: [...] } -|  (表示用行データ)
```

### 新規API: `get_display_rows`

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

### フロントエンドの変更

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

### スクロール機能仕様

#### スクロールモード

| モード | 動作 |
|--------|------|
| **Auto-scroll ON** | スクロール位置を常に下端に固定。新規データ追加時も下端を維持 |
| **Auto-scroll OFF** | ユーザー操作のみ。表示バイト位置を維持 |

#### 定数 (viewerConstants.ts)

| 定数 | 値 | 備考 |
|------|-----|------|
| `ROW_HEIGHT` | 20px | 行の高さ |
| `BUFFER_ROWS` | 50 | 上下バッファ行数 |
| `MAX_SCROLL_HEIGHT` | 10,000,000px | スケーリング閾値 |
| `THROTTLE_MS` | 100ms | スクロール更新間隔 |
| `BYTES_PER_ROW` | 16 | Hex用 |

#### バイトオフセット変換 (scrollUtils.ts)

```typescript
byteOffset = (scrollTop / scrollHeight) * totalBytes
scrollTop = (byteOffset / totalBytes) * scrollHeight
```

#### モード切り替え時

| 切り替え | 動作 |
|---------|------|
| Hex ↔ ASCII | バイトオフセット維持 |
| Auto ON → OFF | その瞬間の最新バイト位置を記憶 |
| Auto OFF → ON | 次回データ受信時に下端へ |

#### 実装ファイル

| ファイル | 責務 |
|---------|------|
| `scrollUtils.ts` | バイトオフセット⇔scrollTop変換関数 |
| `useByteScroll.ts` | バイトベースのスクロール hook |
| `HexViewer.tsx` | Hex表示（16バイト/行） |
| `AsciiViewer.tsx` | ASCII表示（Text列を主スクロール） |

### スケーリング仮想スクロール（実装済み）

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

### メリット

1. **メモリ効率:** フロントエンドは常に~35行分のデータのみ保持
2. **OOM解消:** 全行配列（200万要素）を生成しない
3. **Hex/ASCII変換のオフロード:** Rust側で高速に処理
4. **DOM制限回避:** スケーリングで2M行以上も正常表示
5. **高速受信時の安定性:** スロットリングで React 負荷を軽減
6. **将来の拡張:** 検索、フィルタリング等もバックエンドで処理可能

## リスク管理 (Disk I/O Bottleneck)

*   **リスク:** ディスク書き込み速度が受信速度 (12Mbps) を下回ると、`free_pool` が枯渇する。
*   **対策:** 十分な数のチャンク（数秒〜数十秒分）を確保し、一時的な遅延を吸収する。

---

## 関連ドキュメント

- [システムアーキテクチャ](02_architecture.md)
- [API仕様](04_api.md)
