# シリアルプロッタ レビュー＆リファクタリング方針

**レビュー日時:** 2025-12-30  
**最終更新:** 2025-12-30

## 概要

シリアルプロッタ機能の設計と実装をレビューし、リファクタリングの方針を整理した。

---

## レビュー対象ファイル

| カテゴリ | ファイル | 行数 |
|----------|----------|------|
| **仕様書** | `docs/07_plotter_spec.md` | 532 |
| **Backend** | `src-tauri/src/plotter/data_store.rs` | 585 |
| **Backend** | `src-tauri/src/plotter/parser.rs` | 474 |
| **Backend** | `src-tauri/src/plotter/thread.rs` | 101 |
| **Frontend** | `src/components/plotter/LineChart.tsx` | 378 |
| **Frontend** | `src/components/plotter/PlotterWindow.tsx` | 194 |
| **Frontend** | `src/components/plotter/stateTimelinePlugin.ts` | 223 |

---

## ⚠️ 改善推奨項目

以下のリファクタリングを推奨する。優先度順に記載。

---

### 1. 【高】未実装機能の明確化（Phase D）

**現状:** 仕様書 `07_plotter_spec.md` の Phase D（設定・最適化）が未実装のまま。

**具体的な未実装項目:**
- 7-7. 設定UI（チャンネル選択、表示時間幅、Y軸レンジ、間引きモード選択）
- 7-8. パフォーマンスチューニング（12Mbps対応）
- 7-9. ドッキングシステム

**リファクタリング方針:**
- 仕様書に「未実装」セクションを明記し、TODOを整理する
- 現在のPhase表記 (`Phase B`) をフッターに表示しているのは良い

---

### 2. 【高・重大】毎フレーム全データコピー問題

**現状:**
- `get_data_payload()` では **毎回全データをクローン** してフロントエンドへ送信
- 100ms間隔で16チャンネル × 10,000ポイント = 約3.2MBの転送が発生する可能性
- `AggregationMode` (Average/MinMax/LTTB) は定義済みだが未使用

```rust
// data_store.rs - 問題のコード
let line_data: HashMap<String, Vec<(u64, f64)>> = inner
    .line_data
    .iter()
    .map(|(k, v)| (k.clone(), v.iter().cloned().collect()))  // ← 全データをコピー
    .collect();
```

**影響:** 高速データ受信時にUIがフリーズ、メモリ使用量増大。

---

## 7-8. パフォーマンス・チューニング設計

> [!CAUTION]
> 以下の設計方針は今後の実装で遵守すること。

### 設計概要

シンプルで効率的なデータ管理を実現する：

1. **動的集約** - データ量が閾値を超えたら自動的に間引き（固定レベル不要）
2. **表示範囲フィルタリング** - 可視領域のデータのみ送信
3. **ズーム時再集約** - スケール変更時はストレージから再読み込みして再集約

```text
┌─────────────────────────────────────────────────────────────────┐
│                    データフロー全体像                            │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  [受信データ] ──→ [ストレージ] ──→ [オンデマンド集約]           │
│                       │                   │                     │
│                  全データ保持        表示時に集約               │
│                       │                   │                     │
│                       ▼                   ▼                     │
│              ┌─────────────────┐    ┌─────────────────┐        │
│              │ 生データ保持   │    │ 表示範囲抽出    │        │
│              │ (リングバッファ)│ →  │ + 動的集約      │ → FE   │
│              │               │    │ 最大4000点に制限│        │
│              └─────────────────┘    └─────────────────┘        │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

---

### 設計原則

> [!IMPORTANT]
> 以下の原則を遵守すること。

| 原則 | 説明 |
|------|------|
| **固定レベル不使用** | `LEVEL_CONFIGS` のような固定階層は使用しない |
| **動的集約** | データ量が表示量の一定倍（例: 2倍）を超えたら集約 |
| **ズーム時再集約** | スケール変更時はストレージの全データを参照して再集約 |
| **4K対応** | 最大ポイント数は 4000 程度（4K横幅対応） |

---

### 1. データストア設計

#### ストレージ層（生データ保持）

```rust
/// プロッタ用データストア
pub struct PlotterDataStore {
    /// チャンネルごとの生データ（リングバッファ）
    /// 高速受信時でも一定量の生データを保持
    raw_data: HashMap<String, VecDeque<(u64, f64)>>,
    
    /// 現在表示用にキャッシュされた集約データ
    /// ズーム/パン変更時にクリアされる
    cached_aggregated: Option<CachedAggregation>,
    
    /// 設定
    config: PlotterConfig,
}

/// キャッシュされた集約結果
struct CachedAggregation {
    /// キャッシュ生成時の表示範囲
    time_range: (f64, f64),
    /// キャッシュ生成時のピクセル幅
    pixel_width: u32,
    /// 集約済みデータ
    data: HashMap<String, Vec<(u64, f64)>>,
}
```

#### 集約済みバケット（集約処理用）

```rust
/// 集約済みバケット
#[derive(Debug, Clone)]
pub struct AggregatedBucket {
    /// バケット開始時刻（ms）
    pub start_ms: u64,
    /// データポイント数
    pub count: u32,
    /// 合計値（Average計算用）
    pub sum: f64,
    /// 最小値
    pub min: f64,
    /// 最大値
    pub max: f64,
}
```

---

### 2. 動的集約アルゴリズム

#### 集約トリガー

```rust
/// 表示用データを取得（必要に応じて集約）
pub fn get_display_data(
    &self,
    time_min: f64,
    time_max: f64,
    pixel_width: u32,
) -> Vec<(u64, f64)> {
    // 1. 表示範囲内の生データを抽出
    let raw_points = self.extract_range(time_min, time_max);
    
    // 2. 目標ポイント数（4K対応: 最大4000点）
    let target_points = pixel_width.min(4000) as usize;
    
    // 3. 集約が必要かチェック（データ量が目標の2倍以上）
    if raw_points.len() <= target_points * 2 {
        // 集約不要：生データをそのまま返す
        return raw_points;
    }
    
    // 4. 動的にバケット幅を計算して集約
    let bucket_width_ms = calculate_bucket_width(time_min, time_max, target_points);
    aggregate_to_buckets(&raw_points, bucket_width_ms, self.config.aggregation_mode)
}

/// バケット幅を動的に計算
fn calculate_bucket_width(time_min: f64, time_max: f64, target_points: usize) -> u64 {
    let time_range_ms = ((time_max - time_min) * 1000.0) as u64;
    time_range_ms / target_points as u64
}
```

#### 集約処理

```rust
/// データをバケットに集約
fn aggregate_to_buckets(
    data: &[(u64, f64)],
    bucket_width_ms: u64,
    mode: AggregationMode,
) -> Vec<(u64, f64)> {
    let mut buckets: HashMap<u64, AggregatedBucket> = HashMap::new();
    
    for &(ts, val) in data {
        let bucket_key = ts / bucket_width_ms * bucket_width_ms;
        let bucket = buckets.entry(bucket_key).or_insert(AggregatedBucket {
            start_ms: bucket_key,
            count: 0,
            sum: 0.0,
            min: f64::MAX,
            max: f64::MIN,
        });
        bucket.count += 1;
        bucket.sum += val;
        bucket.min = bucket.min.min(val);
        bucket.max = bucket.max.max(val);
    }
    
    // モードに応じて代表値を選択
    let mut result: Vec<(u64, f64)> = buckets.into_iter()
        .map(|(ts, b)| {
            let value = match mode {
                AggregationMode::Average => b.sum / b.count as f64,
                AggregationMode::MinMax => (b.min + b.max) / 2.0,
                AggregationMode::Lttb => b.sum / b.count as f64,
                AggregationMode::None => b.sum / b.count as f64,
            };
            (ts, value)
        })
        .collect();
    
    result.sort_by_key(|(ts, _)| *ts);
    result
}
```

---

### 3. ズーム/パン時の再集約

#### 処理フロー

```text
┌─────────────────────────────────────────────────────────────────┐
│ ズーム/パン操作時の処理                                          │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  1. フロントエンドから新しい表示範囲を受信                       │
│     (time_min, time_max, pixel_width)                           │
│                                                                 │
│  2. キャッシュの有効性チェック                                   │
│     ├─ 表示範囲が変わった → キャッシュ破棄                      │
│     └─ ピクセル幅が大幅に変わった → キャッシュ破棄              │
│                                                                 │
│  3. キャッシュ無効の場合:                                        │
│     ├─ ストレージから表示範囲の全データを読み出し               │
│     ├─ 新しいバケット幅で再集約                                 │
│     └─ 結果をキャッシュに保存                                   │
│                                                                 │
│  4. 集約済みデータをフロントエンドに返す                         │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

#### API 設計

```rust
/// フロントエンドからのリクエスト
#[derive(Debug, Deserialize)]
pub struct PlotterDataRequest {
    /// 表示開始時刻（秒、uPlotのスケール単位）
    pub time_min: f64,
    /// 表示終了時刻（秒）
    pub time_max: f64,
    /// 描画領域の幅（ピクセル）
    pub pixel_width: u32,
}

/// レスポンス生成
pub fn get_plotter_data_ranged(req: PlotterDataRequest) -> PlotterDataPayload {
    // キャッシュチェック
    if !self.is_cache_valid(&req) {
        // ストレージから再読み込み＆再集約
        self.invalidate_cache();
        self.rebuild_cache(&req);
    }
    
    // キャッシュからデータ返却
    self.get_cached_data()
}
```

---

### 4. フロントエンド連携

#### 表示範囲通知

```typescript
// LineChart.tsx - ズーム/パン時に表示範囲を通知
const onTimeRangeChange = useCallback((min: number, max: number) => {
  // デバウンス処理で過剰なリクエストを防止（200ms）
  debouncedFetch({ 
    time_min: min, 
    time_max: max, 
    pixel_width: containerRef.current?.clientWidth ?? 1000 
  });
}, []);
```

#### データ取得

```typescript
// PlotterWindow.tsx
const fetchData = useCallback(async () => {
  const payload = await invoke<PlotterDataPayload>('get_plotter_data_ranged', {
    request: {
      time_min: visibleRange.min,
      time_max: visibleRange.max,
      pixel_width: chartWidth,
    }
  });
  setData(payload);
}, [visibleRange, chartWidth]);
```

---

### 5. パフォーマンス見積もり

| シナリオ | 現在の実装 | 改善後 |
|----------|-----------|--------|
| 10秒表示 × 4ch (4K) | 最大 40,000点転送 | 最大 16,000点 |
| 1時間表示 × 16ch | 160,000点転送 | 最大 64,000点 |
| ズーム操作 | 毎回全データ転送 | 再集約（数ms） |

---

### 6. 実装ステップ

| Step | 内容 | 優先度 |
|------|------|--------|
| 6-1 | `get_plotter_data_ranged` API 実装 | 高 |
| 6-2 | 動的バケット幅計算 + 集約処理実装 | 高 |
| 6-3 | フロントエンドの表示範囲通知実装（デバウンス付き） | 高 |
| 6-4 | Average/MinMax 集約モード実装 | 高 |
| 6-5 | LTTB 集約モード実装 | 中 |
| 6-6 | パフォーマンステスト（12Mbps） | 高 |

**優先度が高い理由:** 仕様書の「12Mbps対応」は現在の実装では不可能。

---

### 3. 【中】PlotterWindow の Polling 方式

**現状:** `setInterval(fetchData, 100)` で10Hzポーリング。

**考慮事項:**
- 仕様書ではイベント駆動方式を推奨している
- 現状でも動作はするが、データ更新がない時でもリクエストが発生

**リファクタリング方針:**
- Phase D（パフォーマンス最適化）でTauriイベント方式に移行
- 現状は許容範囲内（10Hzは負荷として軽微）

---

## まとめ

現在の実装は **Phase A〜C が完了** しており、基本機能は問題なく動作する。

### 次フェーズで対応すべき項目
1. 間引き処理（LTTB等）の実装
2. 設定UI（チャンネル選択、表示設定等）
3. パフォーマンスチューニング

