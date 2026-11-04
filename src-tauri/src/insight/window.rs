//! ウィンドウ指定の解決とカーソルの生成 / 解釈（純粋関数）
//!
//! docs/26 §3「時計は 1 つ、アドレス空間は 1 つ」を実装する場所。
//!
//! - 唯一の正のアドレスは**ストリーム絶対バイトオフセット**。
//! - 時刻は `DataStore` の `timestamp_index`（実 epoch ms、100 ms 刻み）のみ。
//!   時刻指定のウィンドウは必ずバイト範囲へ解決し、解決結果を応答に明記する
//!   （エージェントが同じ範囲を `serial_read_range` で再導出できる）。
//! - 索引が時刻を持たないとき、時刻指定は**捏造せずに失敗させる**。

/// `timestamp_index` の記録間隔（`UiNotifier`）。時刻解決の誤差として明示する
pub const TIMESTAMP_RESOLUTION_MS: u64 = 100;

/// 時刻 → オフセットを引ける索引の抽象
///
/// 実運用では `serial::DataStore`、試験では固定の表を裏に置く。
pub trait TimeIndex {
    /// 記録済みの (最古, 最新) タイムスタンプ
    fn bounds(&self) -> Option<(u64, u64)>;
    /// `ts` 以降で最初に記録されたオフセット
    fn offset_at_or_after(&self, ts: u64) -> Option<u64>;
}

/// ウィンドウ指定
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowSpec {
    /// 索引の全期間
    All,
    /// 直近 N ミリ秒
    Duration(u64),
    /// バイト範囲（`to` 省略時は末尾まで）
    Offsets { from: u64, to: Option<u64> },
}

/// `"all"` / `"30s"` / `"5m"` / `"1h"` / `"1500ms"` / `"off:1000..2000"` を解釈する
pub fn parse_window(spec: &str) -> Result<WindowSpec, String> {
    let s = spec.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("all") {
        return Ok(WindowSpec::All);
    }
    if let Some(rest) = s.strip_prefix("off:") {
        let (from, to) = match rest.split_once("..") {
            Some((a, b)) => (a.trim(), b.trim()),
            None => (rest.trim(), ""),
        };
        let from = from
            .parse::<u64>()
            .map_err(|_| format!("bad window: '{spec}' (expected off:FROM..TO)"))?;
        let to = if to.is_empty() {
            None
        } else {
            Some(
                to.parse::<u64>()
                    .map_err(|_| format!("bad window: '{spec}' (expected off:FROM..TO)"))?,
            )
        };
        if let Some(to) = to {
            if to < from {
                return Err(format!("bad window: '{spec}' (to < from)"));
            }
        }
        return Ok(WindowSpec::Offsets { from, to });
    }
    // 時間指定。単位の長い順に見る（"ms" を "m" より先に）
    for (suffix, scale) in [("ms", 1u64), ("s", 1000), ("m", 60_000), ("h", 3_600_000)] {
        if let Some(head) = s.strip_suffix(suffix) {
            let value = head.trim().parse::<u64>().map_err(|_| {
                format!("bad window: '{spec}' (expected a number before '{suffix}')")
            })?;
            if value == 0 {
                return Err(format!("bad window: '{spec}' (duration must be > 0)"));
            }
            return Ok(WindowSpec::Duration(value.saturating_mul(scale)));
        }
    }
    Err(format!(
        "bad window: '{spec}' (use \"all\", a duration like \"30s\"/\"5m\", or \"off:FROM..TO\")"
    ))
}

/// 解決済みウィンドウ
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWindow {
    pub start: u64,
    pub end: u64,
    /// 応答へそのまま載せる説明（エージェントが再導出できるように）
    pub label: String,
    /// 時刻から解決した（`±TIMESTAMP_RESOLUTION_MS` の誤差を持つ）
    pub time_based: bool,
}

/// ウィンドウ指定をバイト範囲へ解決する
///
/// `index_start` は索引がこの世代で取り込みを始めたオフセット。索引より前は
/// 集計できないので、そこで下限を切って `label` に明記する。
pub fn resolve_window(
    spec: &WindowSpec,
    index_start: u64,
    index_end: u64,
    time: &dyn TimeIndex,
) -> Result<ResolvedWindow, String> {
    let clamp = |v: u64| v.clamp(index_start, index_end);
    match spec {
        WindowSpec::All => Ok(ResolvedWindow {
            start: index_start,
            end: index_end,
            label: format!("window=all -> off {index_start}..{index_end}"),
            time_based: false,
        }),
        WindowSpec::Offsets { from, to } => {
            let start = clamp(*from);
            let end = clamp(to.unwrap_or(index_end));
            Ok(ResolvedWindow {
                start,
                end: end.max(start),
                label: format!("window=off:{from}.. -> off {start}..{}", end.max(start)),
                time_based: false,
            })
        }
        WindowSpec::Duration(ms) => {
            let (_first_ts, last_ts) = time.bounds().ok_or_else(|| {
                "no timing data yet; use window=\"all\" or an offset window like off:1000..2000"
                    .to_string()
            })?;
            // 「今」は壁時計ではなく**最後に記録された時刻**。受信が止まって
            // いるときに壁時計を使うと、空のウィンドウを返して「何も起きて
            // いない」と誤解させる。
            let target = last_ts.saturating_sub(*ms);
            let start = clamp(time.offset_at_or_after(target).unwrap_or(index_start));
            Ok(ResolvedWindow {
                start,
                end: index_end,
                label: format!(
                    "window={} -> off {start}..{index_end} (+/-{TIMESTAMP_RESOLUTION_MS}ms)",
                    format_duration(*ms)
                ),
                time_based: true,
            })
        }
    }
}

/// 3600000 -> "1h" のような短い表記に戻す
pub fn format_duration(ms: u64) -> String {
    if ms.is_multiple_of(3_600_000) {
        format!("{}h", ms / 3_600_000)
    } else if ms.is_multiple_of(60_000) {
        format!("{}m", ms / 60_000)
    } else if ms.is_multiple_of(1000) {
        format!("{}s", ms / 1000)
    } else {
        format!("{ms}ms")
    }
}

// ============================================================================
// カーソル
// ============================================================================

/// 追随用ハンドル。**世代 `gen` を内包する**（docs/26 §3.2）
///
/// 世代が変わった（Clear / ポート再オープン / 巻き戻り）カーソルは
/// **エラーにしない**。世代が変わった事実と新しい開始位置を返す方が、
/// 購読側が静かに嘘のデルタを受け取るよりずっと安全。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub gen: u64,
    pub offset: u64,
    pub lines: u64,
    pub epoch_ms: u64,
}

impl Cursor {
    pub fn encode(&self) -> String {
        format!(
            "v1:g{}:o{}:l{}:e{}",
            self.gen, self.offset, self.lines, self.epoch_ms
        )
    }
}

/// `v1:g3:o134612044:l4286084:e1756900000` を解釈する
pub fn parse_cursor(text: &str) -> Result<Cursor, String> {
    let bad = || format!("bad cursor: '{text}' (pass back the cursor from a previous reply)");
    let mut parts = text.trim().split(':');
    if parts.next() != Some("v1") {
        return Err(bad());
    }
    let mut field = |prefix: char| -> Result<u64, String> {
        parts
            .next()
            .and_then(|p| p.strip_prefix(prefix))
            .and_then(|n| n.parse::<u64>().ok())
            .ok_or_else(bad)
    };
    let gen = field('g')?;
    let offset = field('o')?;
    let lines = field('l')?;
    let epoch_ms = field('e')?;
    if parts.next().is_some() {
        return Err(bad());
    }
    Ok(Cursor {
        gen,
        offset,
        lines,
        epoch_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// (timestamp, cumulative_bytes) の昇順表
    struct FakeTime(Vec<(u64, u64)>);

    impl TimeIndex for FakeTime {
        fn bounds(&self) -> Option<(u64, u64)> {
            Some((self.0.first()?.0, self.0.last()?.0))
        }
        fn offset_at_or_after(&self, ts: u64) -> Option<u64> {
            self.0.iter().find(|(t, _)| *t >= ts).map(|(_, o)| *o)
        }
    }

    struct NoTime;
    impl TimeIndex for NoTime {
        fn bounds(&self) -> Option<(u64, u64)> {
            None
        }
        fn offset_at_or_after(&self, _ts: u64) -> Option<u64> {
            None
        }
    }

    #[test]
    fn test_parse_window_forms() {
        assert_eq!(parse_window("all").unwrap(), WindowSpec::All);
        assert_eq!(parse_window("").unwrap(), WindowSpec::All);
        assert_eq!(parse_window("30s").unwrap(), WindowSpec::Duration(30_000));
        assert_eq!(parse_window("5m").unwrap(), WindowSpec::Duration(300_000));
        assert_eq!(parse_window("1h").unwrap(), WindowSpec::Duration(3_600_000));
        assert_eq!(parse_window("250ms").unwrap(), WindowSpec::Duration(250));
        assert_eq!(
            parse_window("off:10..20").unwrap(),
            WindowSpec::Offsets {
                from: 10,
                to: Some(20)
            }
        );
        assert_eq!(
            parse_window("off:10").unwrap(),
            WindowSpec::Offsets { from: 10, to: None }
        );
        assert!(parse_window("yesterday").is_err());
        assert!(parse_window("0s").is_err());
        assert!(parse_window("off:20..10").is_err());
    }

    #[test]
    fn test_time_window_resolves_to_offsets() {
        let time = FakeTime(vec![
            (1000, 0),
            (1100, 5_000),
            (1200, 12_000),
            (1300, 20_000),
        ]);
        let w = resolve_window(&WindowSpec::Duration(200), 0, 20_000, &time).unwrap();
        // 「今」は最後の記録時刻 1300。1300-200=1100 以降の最初のオフセット
        assert_eq!(w.start, 5_000);
        assert_eq!(w.end, 20_000);
        assert!(w.time_based);
        assert!(w.label.contains("off 5000..20000"), "{}", w.label);
        assert!(w.label.contains("100ms"), "{}", w.label);
    }

    #[test]
    fn test_time_window_without_timestamps_fails_loudly() {
        let err = resolve_window(&WindowSpec::Duration(1000), 0, 100, &NoTime).unwrap_err();
        assert!(err.contains("no timing data"), "{err}");
        // 時刻が無くても all / offset は使える
        assert!(resolve_window(&WindowSpec::All, 0, 100, &NoTime).is_ok());
        assert!(
            resolve_window(&WindowSpec::Offsets { from: 5, to: None }, 0, 100, &NoTime).is_ok()
        );
    }

    #[test]
    fn test_offset_window_is_clamped_to_the_indexed_range() {
        let w = resolve_window(
            &WindowSpec::Offsets {
                from: 0,
                to: Some(u64::MAX),
            },
            500,
            900,
            &NoTime,
        )
        .unwrap();
        // 索引していない領域は集計できないので、索引範囲へ切り詰める
        assert_eq!((w.start, w.end), (500, 900));
    }

    #[test]
    fn test_cursor_round_trip() {
        let c = Cursor {
            gen: 3,
            offset: 134_612_044,
            lines: 4_286_084,
            epoch_ms: 1_756_900_000,
        };
        assert_eq!(c.encode(), "v1:g3:o134612044:l4286084:e1756900000");
        assert_eq!(parse_cursor(&c.encode()).unwrap(), c);
        for bad in [
            "",
            "v2:g1:o1:l1:e1",
            "v1:g1:o1:l1",
            "v1:g1:o1:l1:e1:x",
            "junk",
        ] {
            assert!(parse_cursor(bad).is_err(), "{bad} should not parse");
        }
    }

    proptest! {
        /// カーソルは往復して同じ値になる（追随の唯一のハンドル）
        #[test]
        fn prop_cursor_round_trips(gen in 0u64..1_000, offset in 0u64..u64::MAX, lines in 0u64..u64::MAX, epoch in 0u64..u64::MAX) {
            let c = Cursor { gen, offset, lines, epoch_ms: epoch };
            prop_assert_eq!(parse_cursor(&c.encode()).unwrap(), c);
        }

        /// 解決結果は必ず索引範囲に収まり、start <= end（負の幅を作らない）
        #[test]
        fn prop_resolved_window_stays_inside_the_index(
            from in 0u64..10_000, to in 0u64..10_000, start in 0u64..5_000, len in 0u64..5_000,
        ) {
            let index_start = start;
            let index_end = start + len;
            let spec = WindowSpec::Offsets { from, to: Some(from.max(to)) };
            let w = resolve_window(&spec, index_start, index_end, &NoTime).unwrap();
            prop_assert!(w.start >= index_start && w.start <= index_end);
            prop_assert!(w.end >= index_start && w.end <= index_end);
            prop_assert!(w.start <= w.end);
        }
    }
}
