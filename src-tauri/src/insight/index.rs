//! テンプレート表・重大度・チェックポイントを持つ索引本体
//!
//! docs/26 §2.2 のとおり、この索引は**ブリッジの所有物**であり、プロッタ窓や
//! メイン窓の開閉に影響されない。GUI のライフサイクルに縛られた集約器を流用
//! すると、人間がプロット窓を閉じただけでエージェントの答えが遡って変わる。
//!
//! # メモリ
//!
//! 取り込み量に比例して増える構造を持たない。テンプレート表は
//! [`MAX_TEMPLATES`] で有界（追い出しは必ず `evicted` として報告する）、
//! チェックポイント環は [`MAX_CHECKPOINTS`] で有界、行の途中は
//! [`MAX_PARTIAL_LINE`] で打ち切る。
//!
//! # 正確さ
//!
//! - 重大度はテンプレート表と独立に**全バイト**を走査するので、
//!   `coverage == 100%` である限り件数は正確（docs/26 §6.2-3）。
//! - 欠落（読めなかった領域）は `gap_bytes` に積み、隠さない。
//! - 追い出したテンプレートの行数は `evicted_lines` に残すので、
//!   「TOP の合計と総行数が合わない」を黙って起こさない。

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use super::mask::{mask_line, scan_severity, Slot, SlotKind, SEVERITY_CARRY};
use super::window::Cursor;

/// テンプレート表の上限。超えたら最も長く見ていないものを追い出す
pub const MAX_TEMPLATES: usize = 256;
/// チェックポイント環の長さ
pub const MAX_CHECKPOINTS: usize = 96;
/// 自動チェックポイントの最小間隔
pub const CHECKPOINT_INTERVAL_MS: u64 = 1000;
/// 改行が来ないまま溜め込む上限（バイナリ流入で無制限に伸びないように）
pub const MAX_PARTIAL_LINE: usize = 64 * 1024;
/// SAMPLE 用に保持する直近の行数
pub const RECENT_LINES: usize = 8;
/// SAMPLE 1 行の最大文字数
pub const RECENT_LINE_CHARS: usize = 200;
/// 「行指向ではなさそう」と言い始める制御バイトの割合
///
/// 制御バイト = タブ / CR / LF / 表示可能 ASCII / 0x80 以上 のいずれでもないもの。
/// 0x80 以上を可とするのは、CJK ログをバイナリと誤認しないため（誤検知の方が
/// 害が大きい）。一様乱数のバイナリはこの定義で約 11% になるので、5% は
/// 「テキストではまず起きないが、バイナリなら確実に超える」線。
pub const BINARY_HINT_RATIO: f64 = 0.05;
/// この量を取り込むまでは判定しない（数バイトで断定しない）
pub const BINARY_HINT_MIN_BYTES: u64 = 256;

/// テンプレートごとに覚えておく直近の出現オフセット数
///
/// 生の行そのものは持たない（256 テンプレート分の本文はメモリを食う）。
/// オフセットだけ覚えておけば `serial_sample(by_template)` が必要なときに
/// `DataStore` から読み直せる。4 x 8 バイト x 256 = 8 KB。
pub const TEMPLATE_RECENT_OFFSETS: usize = 4;

/// 1 スロットの統計
#[derive(Debug, Clone, PartialEq)]
pub struct SlotStat {
    pub kind: SlotKind,
    /// 数値スロットのみ有効
    pub numeric: bool,
    pub min: f64,
    pub max: f64,
    pub last: f64,
    /// 最小値 / 最大値が観測されたオフセット（極値には必ずハンドルを付ける）
    pub min_offset: u64,
    pub max_offset: u64,
}

/// 1 テンプレートの統計
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateStat {
    pub id: u32,
    pub template: String,
    pub count: u64,
    pub first_offset: u64,
    pub last_offset: u64,
    pub slots: Vec<SlotStat>,
    /// 直近の出現オフセット（古い順、最大 [`TEMPLATE_RECENT_OFFSETS`] 件）
    pub recent_offsets: Vec<u64>,
}

/// ある時点の集計（デルタの基準点）
#[derive(Debug, Clone)]
struct Checkpoint {
    offset: u64,
    lines: u64,
    epoch_ms: u64,
    err: u64,
    warn: u64,
    counts: HashMap<u32, u64>,
}

/// 追随の差分
#[derive(Debug, Clone)]
pub struct Delta {
    pub from_offset: u64,
    pub to_offset: u64,
    pub from_lines: u64,
    pub to_lines: u64,
    /// `to_offset` 時点の実 epoch ms（カーソルに載せる）
    pub to_epoch_ms: u64,
    pub elapsed_ms: u64,
    pub err: u64,
    pub warn: u64,
    /// (統計, 増分, このウィンドウで初出か) を増分の降順で
    pub changed: Vec<(TemplateStat, u64, bool)>,
    /// 要求どおりの基準点が見つかった（false なら `from_offset` は近似）
    pub exact_baseline: bool,
    /// カーソルの世代が現在と違った
    pub gen_changed: bool,
}

/// 索引全体の写し
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub gen: u64,
    pub running: bool,
    /// この世代で取り込みを始めたオフセット
    pub start_offset: u64,
    /// ここまで取り込んだ
    pub indexed_up_to: u64,
    /// スレッドが最後に観測したストアの総バイト数
    pub store_total: u64,
    pub bytes_ingested: u64,
    pub lines: u64,
    pub err: u64,
    pub warn: u64,
    pub gap_bytes: u64,
    pub gaps: u32,
    pub evicted_templates: u64,
    pub evicted_lines: u64,
    /// 制御バイト数（行指向かどうかの判断材料）
    pub control_bytes: u64,
    /// 件数の降順
    pub templates: Vec<TemplateStat>,
    pub first_ingest_ms: u64,
    pub last_ingest_ms: u64,
    /// (offset, 行) の直近サンプル
    pub recent: Vec<(u64, String)>,
    /// 時系列バケット用の素材 (offset, epoch_ms, lines, err, warn)
    pub checkpoints: Vec<(u64, u64, u64, u64, u64)>,
}

impl Snapshot {
    /// 索引が担当範囲をどれだけ読めたか（%）
    ///
    /// 「読めなかった」と「そこには何も無かった」を混同させないための値。
    pub fn coverage_pct(&self) -> u64 {
        let span = self.indexed_up_to.saturating_sub(self.start_offset);
        if span == 0 {
            return 100;
        }
        (self.bytes_ingested.saturating_mul(100) / span).min(100)
    }

    /// 取り込んだバイトのうち制御バイトの割合
    pub fn control_ratio(&self) -> f64 {
        if self.bytes_ingested == 0 {
            return 0.0;
        }
        self.control_bytes as f64 / self.bytes_ingested as f64
    }

    /// テンプレート化した結果を信じてよさそうか
    ///
    /// false のときテンプレートや行数は**意味を持たない**。黙ってそれらしい
    /// 数字を並べると、バイナリのストリームに対して自信満々に嘘をつくことになる。
    pub fn looks_line_oriented(&self) -> bool {
        self.bytes_ingested < BINARY_HINT_MIN_BYTES || self.control_ratio() < BINARY_HINT_RATIO
    }

    /// 索引がストアからどれだけ遅れているか
    pub fn lag_bytes(&self) -> u64 {
        self.store_total.saturating_sub(self.indexed_up_to)
    }

    /// 件数を「正確」と言ってよいか
    pub fn is_exact(&self) -> bool {
        self.gap_bytes == 0 && self.lag_bytes() == 0
    }

    pub fn cursor(&self) -> Cursor {
        Cursor {
            gen: self.gen,
            offset: self.indexed_up_to,
            lines: self.lines,
            epoch_ms: self.last_ingest_ms,
        }
    }
}

#[derive(Default)]
struct Inner {
    gen: u64,
    running: bool,
    start_offset: u64,
    indexed_up_to: u64,
    store_total: u64,
    bytes_ingested: u64,
    gap_bytes: u64,
    gaps: u32,
    lines: u64,
    err: u64,
    warn: u64,
    control_bytes: u64,
    partial: Vec<u8>,
    severity_carry: Vec<u8>,
    templates: HashMap<String, u32>,
    by_id: HashMap<u32, TemplateStat>,
    next_id: u32,
    evicted_templates: u64,
    evicted_lines: u64,
    checkpoints: VecDeque<Checkpoint>,
    last_checkpoint_ms: u64,
    first_ingest_ms: u64,
    last_ingest_ms: u64,
    recent: VecDeque<(u64, String)>,
}

/// 索引。`Arc<InsightIndex>` としてブリッジが所有し、索引スレッドが書き込む
pub struct InsightIndex {
    inner: Mutex<Inner>,
}

impl Default for InsightIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl InsightIndex {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                next_id: 1,
                ..Inner::default()
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // 索引は「答えを返せなくなる」より「毒された状態でも答える」方がよい
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 索引スレッドの開始 / 停止を記録する（`running` は応答に明記される）
    pub fn set_running(&self, running: bool) {
        self.lock().running = running;
    }

    /// 世代交代（Clear / ポート再オープン / ストア差し替え）
    ///
    /// 集計を捨て、新しい世代番号を振る。古いカーソルは
    /// [`Self::delta_since`] が `gen_changed` として報告する。
    pub fn reset(&self, start_offset: u64) -> u64 {
        let mut inner = self.lock();
        let gen = inner.gen + 1;
        let running = inner.running;
        *inner = Inner {
            gen,
            running,
            start_offset,
            indexed_up_to: start_offset,
            next_id: 1,
            ..Inner::default()
        };
        gen
    }

    /// スレッドが観測したストアの総バイト数を記録する（遅れの計算用）
    pub fn note_store_total(&self, total: u64) {
        let mut inner = self.lock();
        inner.store_total = inner.store_total.max(total).max(inner.indexed_up_to);
    }

    /// 読めなかった領域を飛ばした
    pub fn note_gap(&self, bytes: u64) {
        let mut inner = self.lock();
        if bytes == 0 {
            return;
        }
        inner.gap_bytes += bytes;
        inner.gaps += 1;
        inner.indexed_up_to += bytes;
        // 行の途中と重大度の持ち越しは、飛ばした先とは繋がらない
        inner.partial.clear();
        inner.severity_carry.clear();
    }

    /// 受信セグメントの開始（自動再接続の再開位置など）。直前の途中の行は
    /// 次のセグメントの先頭とつながっていないので捨てる（docs/27 PF-2）
    pub fn note_segment_start(&self) {
        let mut inner = self.lock();
        inner.partial.clear();
        inner.severity_carry.clear();
    }

    /// 取り込み。`offset` は `data` の先頭のストリーム絶対オフセット
    pub fn ingest(&self, offset: u64, data: &[u8], now_ms: u64) {
        if data.is_empty() {
            return;
        }
        let mut inner = self.lock();
        if offset != inner.indexed_up_to {
            // 呼び出し側が飛ばしたぶんを申告していない。黙って詰めない
            let skipped = offset.saturating_sub(inner.indexed_up_to);
            if skipped > 0 {
                inner.gap_bytes += skipped;
                inner.gaps += 1;
            }
            inner.partial.clear();
            inner.severity_carry.clear();
            inner.indexed_up_to = offset;
        }
        if inner.first_ingest_ms == 0 {
            inner.first_ingest_ms = now_ms;
        }
        inner.last_ingest_ms = now_ms;
        inner.bytes_ingested += data.len() as u64;
        inner.indexed_up_to += data.len() as u64;
        inner.store_total = inner.store_total.max(inner.indexed_up_to);

        // --- 行指向かどうかの材料。重大度と同じ 1 パスで数えられる安さ ---
        inner.control_bytes += data
            .iter()
            .filter(|&&b| {
                !(b == b'\t' || b == b'\n' || b == b'\r' || (0x20..=0x7e).contains(&b) || b >= 0x80)
            })
            .count() as u64;

        // --- 重大度: テンプレート表と独立に全バイトを走査する ---
        let carry = std::mem::take(&mut inner.severity_carry);
        let mut scan_buf = carry;
        let carry_len = scan_buf.len();
        scan_buf.extend_from_slice(data);
        let counts = scan_severity(&scan_buf, carry_len);
        inner.err += counts.err;
        inner.warn += counts.warn;
        let keep = scan_buf.len().saturating_sub(SEVERITY_CARRY);
        inner.severity_carry = scan_buf[keep..].to_vec();

        // --- 行の切り出し。LF だけでなく CR 単独終端も行とみなす ---
        // （`DataStore::record_line_offsets` は LF のみを記録するため、
        //   CR 終端デバイスでは行番号が永遠に 1 になる。docs/26 §1）
        let mut line_start = 0usize;
        let mut line_offset = offset.saturating_sub(inner.partial.len() as u64);
        for i in 0..data.len() {
            if data[i] != b'\n' && data[i] != b'\r' {
                continue;
            }
            let mut line = std::mem::take(&mut inner.partial);
            line.extend_from_slice(&data[line_start..i]);
            if !line.is_empty() {
                Self::record_line(&mut inner, line_offset, &line);
            }
            line_start = i + 1;
            line_offset = offset + line_start as u64;
        }
        let tail = &data[line_start..];
        if !tail.is_empty() {
            if inner.partial.len() + tail.len() > MAX_PARTIAL_LINE {
                // 改行が来ない。溜め込みは有界に保ち、1 行として確定させる
                let mut line = std::mem::take(&mut inner.partial);
                line.extend_from_slice(tail);
                Self::record_line(&mut inner, line_offset, &line);
            } else {
                inner.partial.extend_from_slice(tail);
            }
        }

        // --- 自動チェックポイント（窓つき集計と時系列の素材） ---
        if now_ms.saturating_sub(inner.last_checkpoint_ms) >= CHECKPOINT_INTERVAL_MS {
            Self::push_checkpoint(&mut inner, now_ms);
        }
    }

    fn record_line(inner: &mut Inner, offset: u64, line: &[u8]) {
        inner.lines += 1;
        let masked = mask_line(line);

        if inner.recent.len() == RECENT_LINES {
            inner.recent.pop_front();
        }
        inner.recent.push_back((
            offset,
            String::from_utf8_lossy(line)
                .chars()
                .take(RECENT_LINE_CHARS)
                .map(|c| if (c as u32) < 0x20 { '.' } else { c })
                .collect(),
        ));

        if let Some(&id) = inner.templates.get(&masked.template) {
            if let Some(stat) = inner.by_id.get_mut(&id) {
                stat.count += 1;
                stat.last_offset = offset;
                if stat.recent_offsets.len() == TEMPLATE_RECENT_OFFSETS {
                    stat.recent_offsets.remove(0);
                }
                stat.recent_offsets.push(offset);
                merge_slots(&mut stat.slots, &masked.slots, offset);
                return;
            }
        }
        if inner.templates.len() >= MAX_TEMPLATES {
            Self::evict_one(inner);
        }
        let id = inner.next_id;
        inner.next_id += 1;
        inner.templates.insert(masked.template.clone(), id);
        inner.by_id.insert(
            id,
            TemplateStat {
                id,
                template: masked.template,
                count: 1,
                first_offset: offset,
                last_offset: offset,
                slots: masked.slots.iter().map(|s| new_slot(s, offset)).collect(),
                recent_offsets: vec![offset],
            },
        );
    }

    /// 最も長く見ていないテンプレートを追い出す
    ///
    /// 最も**稀な**ものを追い出すと docs/26 §6.2-2「稀少イベントの床」と
    /// 真っ向から矛盾するので採らない。支配的な行は常に直近なので生き残り、
    /// 落ちるのは「古くて以後現れないもの」になる。追い出した事実と行数は
    /// `evicted` として必ず報告する。
    fn evict_one(inner: &mut Inner) {
        let victim = inner
            .by_id
            .values()
            .min_by_key(|s| (s.last_offset, s.id))
            .map(|s| (s.id, s.template.clone(), s.count));
        if let Some((id, template, count)) = victim {
            inner.by_id.remove(&id);
            inner.templates.remove(&template);
            inner.evicted_templates += 1;
            inner.evicted_lines += count;
            for cp in inner.checkpoints.iter_mut() {
                cp.counts.remove(&id);
            }
        }
    }

    fn push_checkpoint(inner: &mut Inner, now_ms: u64) {
        if inner
            .checkpoints
            .back()
            .is_some_and(|c| c.offset == inner.indexed_up_to)
        {
            inner.last_checkpoint_ms = now_ms;
            return;
        }
        if inner.checkpoints.len() == MAX_CHECKPOINTS {
            inner.checkpoints.pop_front();
        }
        inner.checkpoints.push_back(Checkpoint {
            offset: inner.indexed_up_to,
            lines: inner.lines,
            epoch_ms: now_ms,
            err: inner.err,
            warn: inner.warn,
            counts: inner.by_id.iter().map(|(id, s)| (*id, s.count)).collect(),
        });
        inner.last_checkpoint_ms = now_ms;
    }

    pub fn snapshot(&self) -> Snapshot {
        Self::snapshot_locked(&self.lock())
    }

    /// 写しを取り、**同じロックの中で**その位置にチェックポイントを打つ
    ///
    /// 写しと手渡すカーソルがずれると、次の `watch` が「基準点が無い」と
    /// 言うか、間の行をどちらの応答にも載せないという取りこぼしが起きる。
    pub fn snapshot_with_checkpoint(&self, now_ms: u64) -> (Snapshot, Cursor) {
        let mut inner = self.lock();
        Self::push_checkpoint(&mut inner, now_ms);
        let snap = Self::snapshot_locked(&inner);
        let cursor = snap.cursor();
        (snap, cursor)
    }

    fn snapshot_locked(inner: &Inner) -> Snapshot {
        let mut templates: Vec<TemplateStat> = inner.by_id.values().cloned().collect();
        templates.sort_by(|a, b| b.count.cmp(&a.count).then(a.id.cmp(&b.id)));
        Snapshot {
            gen: inner.gen,
            running: inner.running,
            start_offset: inner.start_offset,
            indexed_up_to: inner.indexed_up_to,
            store_total: inner.store_total.max(inner.indexed_up_to),
            bytes_ingested: inner.bytes_ingested,
            lines: inner.lines,
            err: inner.err,
            warn: inner.warn,
            gap_bytes: inner.gap_bytes,
            gaps: inner.gaps,
            evicted_templates: inner.evicted_templates,
            evicted_lines: inner.evicted_lines,
            control_bytes: inner.control_bytes,
            templates,
            first_ingest_ms: inner.first_ingest_ms,
            last_ingest_ms: inner.last_ingest_ms,
            recent: inner.recent.iter().cloned().collect(),
            checkpoints: inner
                .checkpoints
                .iter()
                .map(|c| (c.offset, c.epoch_ms, c.lines, c.err, c.warn))
                .collect(),
        }
    }

    /// カーソル以降の差分を求める
    ///
    /// カーソルのオフセットにチェックポイントが無ければ、**それ以前で最も
    /// 近いもの**を基準にし `exact_baseline=false` を返す。呼び出し側は
    /// 実際に測った基準点を応答に明記しなければならない（黙って別区間の
    /// 差分を返さない）。
    pub fn delta_since(&self, cursor: Option<&Cursor>, now_ms: u64) -> Delta {
        let mut inner = self.lock();
        let gen_changed = cursor.map(|c| c.gen != inner.gen).unwrap_or(false);

        let baseline: Option<Checkpoint> = match cursor {
            Some(c) if !gen_changed => inner
                .checkpoints
                .iter()
                .rev()
                .find(|cp| cp.offset <= c.offset)
                .or_else(|| inner.checkpoints.front())
                .cloned(),
            // 世代が変わった / カーソルなし: 現世代の最初から
            _ => None,
        };
        let exact_baseline = match (&baseline, cursor) {
            (Some(cp), Some(c)) => cp.offset == c.offset,
            _ => cursor.is_none(),
        };

        let (from_offset, from_lines, from_ms, from_err, from_warn, base_counts) = match &baseline {
            Some(cp) => (
                cp.offset,
                cp.lines,
                cp.epoch_ms,
                cp.err,
                cp.warn,
                cp.counts.clone(),
            ),
            None => (
                inner.start_offset,
                0,
                inner.first_ingest_ms,
                0,
                0,
                HashMap::new(),
            ),
        };

        let mut changed: Vec<(TemplateStat, u64, bool)> = inner
            .by_id
            .values()
            .filter_map(|stat| {
                let before = base_counts.get(&stat.id).copied().unwrap_or(0);
                let delta = stat.count.saturating_sub(before);
                if delta == 0 {
                    return None;
                }
                Some((stat.clone(), delta, !base_counts.contains_key(&stat.id)))
            })
            .collect();
        changed.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.id.cmp(&b.0.id)));

        let delta = Delta {
            from_offset,
            to_offset: inner.indexed_up_to,
            from_lines,
            to_lines: inner.lines,
            to_epoch_ms: inner.last_ingest_ms,
            elapsed_ms: inner.last_ingest_ms.saturating_sub(from_ms),
            err: inner.err.saturating_sub(from_err),
            warn: inner.warn.saturating_sub(from_warn),
            changed,
            exact_baseline,
            gen_changed,
        };
        // 次回のためにこの地点を基準として残す
        Self::push_checkpoint(&mut inner, now_ms);
        delta
    }
}

fn new_slot(slot: &Slot, offset: u64) -> SlotStat {
    let value = slot.num.unwrap_or(0.0);
    SlotStat {
        kind: slot.kind,
        numeric: slot.num.is_some(),
        min: value,
        max: value,
        last: value,
        min_offset: offset,
        max_offset: offset,
    }
}

fn merge_slots(stats: &mut Vec<SlotStat>, slots: &[Slot], offset: u64) {
    if stats.len() != slots.len() {
        // テンプレートが同じならスロット数も同じはずだが、万一ずれたら
        // 統計を捨てて作り直す（間違った min/max を出すよりよい）
        *stats = slots.iter().map(|s| new_slot(s, offset)).collect();
        return;
    }
    for (stat, slot) in stats.iter_mut().zip(slots) {
        let Some(value) = slot.num else {
            continue;
        };
        if !stat.numeric {
            *stat = new_slot(slot, offset);
            continue;
        }
        if value < stat.min {
            stat.min = value;
            stat.min_offset = offset;
        }
        if value > stat.max {
            stat.max = value;
            stat.max_offset = offset;
        }
        stat.last = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(index: &InsightIndex, text: &str, at: u64, now_ms: u64) -> u64 {
        index.ingest(at, text.as_bytes(), now_ms);
        at + text.len() as u64
    }

    #[test]
    fn test_templates_collapse_variable_fields() {
        let index = InsightIndex::new();
        feed(
            &index,
            "T: 23.4 C\nT: 24.1 C\nT: 22.0 C\nE: i2c nak addr=0x48\n",
            0,
            1000,
        );
        let snap = index.snapshot();
        assert_eq!(snap.lines, 4);
        assert_eq!(snap.templates.len(), 2);
        assert_eq!(snap.templates[0].template, "T: <F> C");
        assert_eq!(snap.templates[0].count, 3);
        // 極値にはオフセットが付く（要約が事象を隠さないための検証ハンドル）
        let slot = &snap.templates[0].slots[0];
        assert!(slot.numeric);
        assert_eq!(slot.min, 22.0);
        assert_eq!(slot.max, 24.1);
        assert_eq!(slot.max_offset, 10);
    }

    #[test]
    fn test_severity_is_counted_over_all_bytes() {
        let index = InsightIndex::new();
        feed(&index, "E: fatal fault\nW: rx overflow\n", 0, 1000);
        let snap = index.snapshot();
        assert_eq!(snap.err, 1);
        assert_eq!(snap.warn, 1);
        assert!(snap.is_exact());
        assert_eq!(snap.coverage_pct(), 100);
    }

    #[test]
    fn test_lines_split_across_ingests_are_counted_once() {
        let index = InsightIndex::new();
        index.ingest(0, b"T: 1.0 C\nT: 2", 1000);
        index.ingest(13, b".0 C\nT: 3.0 C\n", 1000);
        let snap = index.snapshot();
        assert_eq!(snap.lines, 3);
        assert_eq!(snap.templates.len(), 1);
        assert_eq!(snap.templates[0].count, 3);
    }

    #[test]
    fn test_severity_split_across_ingests_is_counted_once() {
        let index = InsightIndex::new();
        index.ingest(0, b"rx over", 1000);
        index.ingest(7, b"flow now\n", 1000);
        assert_eq!(index.snapshot().warn, 1);
    }

    #[test]
    fn test_cr_only_terminated_lines_are_split() {
        // DataStore の行索引は LF のみを見るので CR 終端では 1 行のままになる。
        // 索引は自前で CR も行区切りとして扱う（docs/26 §1）
        let index = InsightIndex::new();
        feed(&index, "A: 1\rA: 2\rA: 3\r", 0, 1000);
        assert_eq!(index.snapshot().lines, 3);
    }

    #[test]
    fn test_gap_is_recorded_and_makes_counts_inexact() {
        let index = InsightIndex::new();
        feed(&index, "ok\n", 0, 1000);
        index.note_gap(500);
        feed(&index, "ok\n", 503, 1000);
        let snap = index.snapshot();
        assert_eq!(snap.gap_bytes, 500);
        assert_eq!(snap.gaps, 1);
        assert!(!snap.is_exact());
        assert!(snap.coverage_pct() < 100);
    }

    #[test]
    fn test_unannounced_skip_is_recorded_as_a_gap() {
        let index = InsightIndex::new();
        feed(&index, "ok\n", 0, 1000);
        // 呼び出し側が飛ばしを申告せずに先へ進めても、黙って詰めない
        feed(&index, "ok\n", 1000, 1000);
        let snap = index.snapshot();
        assert_eq!(snap.gaps, 1);
        assert_eq!(snap.gap_bytes, 997);
    }

    #[test]
    fn test_template_table_is_bounded_and_eviction_is_reported() {
        let index = InsightIndex::new();
        let mut at = 0;
        // 全く違う形の行を上限の 2 倍投入する
        for i in 0..(MAX_TEMPLATES * 2) {
            at = feed(&index, &format!("k{i}: value\n"), at, 1000);
        }
        let snap = index.snapshot();
        assert_eq!(snap.templates.len(), MAX_TEMPLATES);
        assert_eq!(snap.evicted_templates, MAX_TEMPLATES as u64);
        assert_eq!(snap.evicted_lines, MAX_TEMPLATES as u64);
        // 行数の総和は追い出し分を足せば合う（黙って消さない）
        let shown: u64 = snap.templates.iter().map(|t| t.count).sum();
        assert_eq!(shown + snap.evicted_lines, snap.lines);
    }

    /// A resume boundary cuts the partial line: "abc" before an unplug and
    /// "def" after the reconnect are not one line "abcdef" (docs/27 PF-2)
    #[test]
    fn test_segment_start_drops_the_partial_line() {
        let index = InsightIndex::new();
        index.ingest(0, b"abc", 1000);
        index.note_segment_start();
        index.ingest(3, b"def\n", 2000);
        let snap = index.snapshot();
        assert_eq!(snap.lines, 1);
        assert!(snap.templates.iter().all(|t| !t.template.contains("abc")));
    }

    #[test]
    fn test_partial_line_is_bounded() {
        let index = InsightIndex::new();
        let blob = vec![b'x'; MAX_PARTIAL_LINE + 10];
        index.ingest(0, &blob, 1000);
        // 改行が来なくても 1 行として確定し、溜め込みは上限を超えない
        assert_eq!(index.snapshot().lines, 1);
    }

    #[test]
    fn test_delta_since_cursor_counts_only_new_lines() {
        let index = InsightIndex::new();
        let mut at = feed(&index, "T: 1.0 C\n", 0, 1000);
        let cursor = index.snapshot_with_checkpoint(1000).1;
        at = feed(&index, "T: 2.0 C\nW: rx overflow\n", at, 2000);
        let delta = index.delta_since(Some(&cursor), 2000);
        assert!(delta.exact_baseline);
        assert!(!delta.gen_changed);
        assert_eq!(delta.to_lines - delta.from_lines, 2);
        assert_eq!(delta.warn, 1);
        assert_eq!(delta.err, 0);
        // 新テンプレートは new として立つ
        let new: Vec<&str> = delta
            .changed
            .iter()
            .filter(|(_, _, is_new)| *is_new)
            .map(|(s, _, _)| s.template.as_str())
            .collect();
        assert_eq!(new, vec!["W: rx overflow"]);
        let _ = at;
    }

    #[test]
    fn test_delta_reports_generation_change_instead_of_lying() {
        let index = InsightIndex::new();
        feed(&index, "A: 1\n", 0, 1000);
        let stale = index.snapshot_with_checkpoint(1000).1;
        index.reset(0);
        feed(&index, "B: 2\n", 0, 2000);
        let delta = index.delta_since(Some(&stale), 2000);
        assert!(delta.gen_changed);
        assert_eq!(delta.from_offset, 0);
        assert_eq!(delta.to_lines, 1);
    }

    #[test]
    fn test_reset_bumps_generation_and_clears_counts() {
        let index = InsightIndex::new();
        feed(&index, "A: 1\nE: fail\n", 0, 1000);
        assert_eq!(index.snapshot().err, 1);
        let gen = index.reset(4096);
        let snap = index.snapshot();
        assert_eq!(snap.gen, gen);
        assert_eq!(snap.err, 0);
        assert_eq!(snap.lines, 0);
        assert_eq!(snap.start_offset, 4096);
        assert_eq!(snap.indexed_up_to, 4096);
    }

    #[test]
    fn test_lag_is_reported_when_the_index_falls_behind() {
        let index = InsightIndex::new();
        feed(&index, "A: 1\n", 0, 1000);
        index.note_store_total(10_000);
        let snap = index.snapshot();
        assert_eq!(snap.lag_bytes(), 10_000 - 5);
        assert!(!snap.is_exact());
    }

    #[test]
    fn test_binary_stream_is_flagged_rather_than_summarised_as_lines() {
        let index = InsightIndex::new();
        // 0x00..0x1f を含む一様なバイト列 = 行指向ではない
        let blob: Vec<u8> = (0..2048u32).map(|i| (i % 256) as u8).collect();
        index.ingest(0, &blob, 1000);
        let snap = index.snapshot();
        assert!(snap.control_ratio() > BINARY_HINT_RATIO);
        assert!(
            !snap.looks_line_oriented(),
            "control ratio {} must not read as line oriented",
            snap.control_ratio()
        );
    }

    #[test]
    fn test_cjk_log_is_not_mistaken_for_binary() {
        let index = InsightIndex::new();
        let mut at = 0;
        for i in 0..40 {
            at = feed(&index, &format!("温度センサ {i}: 23.4 度\n"), at, 1000);
        }
        let _ = at;
        let snap = index.snapshot();
        assert_eq!(snap.control_bytes, 0);
        assert!(snap.looks_line_oriented());
    }

    #[test]
    fn test_a_stray_nul_per_line_is_reported_rather_than_ignored() {
        let index = InsightIndex::new();
        let mut line = b"T: 23.4 C".to_vec();
        line.push(0x00);
        line.push(b'\n');
        let mut at = 0;
        for _ in 0..40 {
            index.ingest(at, &line, 1000);
            at += line.len() as u64;
        }
        let snap = index.snapshot();
        // 11 バイト中 1 バイト = 9% だが、閾値 5% を超えるので警告は出る。
        // 「疑わしいときは黙らない」側に倒す
        assert!(snap.control_ratio() > BINARY_HINT_RATIO);
    }

    #[test]
    fn test_short_captures_are_not_judged_at_all() {
        let index = InsightIndex::new();
        index.ingest(0, &[0u8; 8], 1000);
        // 8 バイトでバイナリと断定しない
        assert!(index.snapshot().looks_line_oriented());
    }
}
