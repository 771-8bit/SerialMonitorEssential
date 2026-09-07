//! セクション組み立てと文字数予算（純粋関数）
//!
//! docs/26 §6.1 の規範をここで**型と順序**として実装する。実行時ヒューリスティック
//! ではなく、セクションの列挙順そのものが優先順位。予算が逼迫したときに削られる
//! のは末尾（サンプル）であって、**アラームは決して削られない**。
//!
//! 予算の適用はこのモジュールの [`render`] 1 箇所だけで行う（docs/26 §2.3）。

use super::index::SlotStat;
use super::mask::SlotKind;

/// フッタ用に先に確保する文字数。本文がどれだけ溢れてもここは落とさない
pub const FOOTER_RESERVE: usize = 140;
/// 封筒（ヘッダ + フッタ）すら入らない予算を許さないための下限
pub const MIN_MAX_CHARS: usize = 500;
pub const MAX_MAX_CHARS: usize = 200_000;
/// `serial_digest` / `serial_watch` の既定予算
pub const DEFAULT_DIGEST_CHARS: usize = 1800;
pub const DEFAULT_WATCH_CHARS: usize = 1200;
/// この件数以下を「稀」として必ず列挙する
pub const RARE_THRESHOLD: u64 = 5;
/// RARE に載せる最大件数（超えたら "N of M shown" と明示する）
pub const RARE_LIMIT: usize = 12;

/// 出力セクション。**この列挙順が表示順であり優先順位**
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Section {
    Header,
    IndexHealth,
    Alarm,
    New,
    Rare,
    Top,
    /// 数値 / 状態テレメトリ。docs/26 段階 5 で埋める枠。順序（＝優先度）を
    /// 先に固定しておくため、中身より先に席だけ用意している
    #[allow(dead_code)]
    Channels,
    Timeline,
    Samples,
    Cursor,
}

impl Section {
    /// 予算に押し出されない固定枠（docs/26 §5「省略による嘘をつかない」）
    pub fn mandatory(self) -> bool {
        matches!(
            self,
            Section::Header | Section::IndexHealth | Section::Alarm | Section::Cursor
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Section::Header => "HEADER",
            Section::IndexHealth => "INDEX",
            Section::Alarm => "ALARM",
            Section::New => "NEW",
            Section::Rare => "RARE",
            Section::Top => "TOP",
            Section::Channels => "CHAN",
            Section::Timeline => "TIME",
            Section::Samples => "SAMPLES",
            Section::Cursor => "CURSOR",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Block {
    pub section: Section,
    pub lines: Vec<String>,
}

impl Block {
    pub fn new(section: Section, lines: Vec<String>) -> Self {
        Self { section, lines }
    }
}

#[derive(Debug, Clone)]
pub struct Rendered {
    pub text: String,
    pub used_chars: usize,
    pub truncated: bool,
    /// 落とした行数（セクション別）。フッタで必ず報告する
    pub withheld: Vec<(Section, usize)>,
}

/// ブロックを優先順位どおりに予算内へ詰める
///
/// 保証:
/// - `text` の文字数は必ず `max_chars` 以下。
/// - フッタは必ず残る。
/// - 落とした行があれば必ずフッタに出る（黙って短くしない）。
pub fn render(blocks: Vec<Block>, next_hint: Option<&str>, max_chars: usize) -> Rendered {
    let max_chars = max_chars.clamp(MIN_MAX_CHARS, MAX_MAX_CHARS);
    let mut blocks = blocks;
    blocks.retain(|b| !b.lines.is_empty());
    blocks.sort_by_key(|b| b.section);

    let budget = max_chars.saturating_sub(FOOTER_RESERVE);
    let width = |s: &str| s.chars().count() + 1; // 改行分

    // 固定枠は先に確保する（落とさない）
    let mut used: usize = blocks
        .iter()
        .filter(|b| b.section.mandatory())
        .flat_map(|b| b.lines.iter())
        .map(|l| width(l))
        .sum();

    let mut kept: Vec<Vec<String>> = blocks
        .iter()
        .map(|b| {
            if b.section.mandatory() {
                b.lines.clone()
            } else {
                Vec::new()
            }
        })
        .collect();
    let mut withheld: Vec<(Section, usize)> = Vec::new();
    let mut full = false;

    for (i, block) in blocks.iter().enumerate() {
        if block.section.mandatory() {
            continue;
        }
        let mut taken = 0usize;
        if !full {
            for line in &block.lines {
                if used + width(line) > budget {
                    full = true; // 以降は優先順位の低い側なので、まとめて落とす
                    break;
                }
                used += width(line);
                kept[i].push(line.clone());
                taken += 1;
            }
        }
        if taken < block.lines.len() {
            withheld.push((block.section, block.lines.len() - taken));
        }
    }

    let body = kept.into_iter().flatten().collect::<Vec<_>>().join("\n");
    let footer = build_footer(&withheld, next_hint);

    // フッタの席は必ず用意する: 溢れたら本文の末尾（＝優先度の低い側）を削る
    let footer_width = footer.chars().count() + 1;
    let mut text = if body.chars().count() + footer_width > max_chars {
        let room = max_chars.saturating_sub(footer_width);
        body.chars().take(room).collect::<String>()
    } else {
        body
    };
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&footer);

    let used_chars = text.chars().count();
    Rendered {
        text,
        used_chars,
        truncated: !withheld.is_empty(),
        withheld,
    }
}

fn build_footer(withheld: &[(Section, usize)], next_hint: Option<&str>) -> String {
    let mut footer = if withheld.is_empty() {
        "- nothing withheld".to_string()
    } else {
        let detail = withheld
            .iter()
            .map(|(s, n)| format!("{} {n}", s.label()))
            .collect::<Vec<_>>()
            .join(", ");
        format!("- withheld: {detail}")
    };
    // ヒントは「落としたものを取り戻す方法」なので、落ちたときだけ出す
    if let (Some(hint), false) = (next_hint, withheld.is_empty()) {
        footer.push_str("; next: ");
        footer.push_str(hint);
    }
    if footer.chars().count() > FOOTER_RESERVE {
        footer = footer.chars().take(FOOTER_RESERVE - 1).collect::<String>();
        footer.push('\u{2026}');
    }
    footer
}

// ============================================================================
// 数の整形（丸めない。桁区切りだけ入れる）
// ============================================================================

/// 402331 -> "402,331"
pub fn commas(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// バイト数を人間向けに（正確な値は必ず別途オフセットで出す）
pub fn human_bytes(n: u64) -> String {
    const UNITS: [(u64, &str); 3] = [
        (1024 * 1024 * 1024, "GB"),
        (1024 * 1024, "MB"),
        (1024, "kB"),
    ];
    for (scale, unit) in UNITS {
        if n >= scale {
            return format!("{:.2} {unit}", n as f64 / scale as f64);
        }
    }
    format!("{n} B")
}

pub fn human_rate(bytes: u64, elapsed_ms: u64) -> String {
    if elapsed_ms == 0 {
        return "n/a".to_string();
    }
    let per_sec = (bytes as f64) * 1000.0 / elapsed_ms as f64;
    format!("{}/s", human_bytes(per_sec as u64))
}

pub fn human_ms(ms: u64) -> String {
    if ms >= 60_000 {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
    } else if ms >= 1000 {
        format!("{:.2}s", ms as f64 / 1000.0)
    } else {
        format!("{ms}ms")
    }
}

/// 丸めない短い数値表記（整数は整数のまま、小数は末尾のゼロを落とす）
pub fn number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.3}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// テンプレートのプレースホルダに観測範囲を書き込む
///
/// `"W: rx overflow q=<N>"` + slot(min=64,max=64) -> `"W: rx overflow q=<N:64>"`。
/// 数値スロットだけを対象にする（16 進や UUID の min/max は意味を持たない）。
pub fn template_with_slots(template: &str, slots: &[SlotStat]) -> String {
    let mut out = String::with_capacity(template.len() + slots.len() * 8);
    let mut rest = template;
    let mut i = 0usize;
    while let Some(pos) = rest.find('<') {
        let Some(close) = rest[pos..].find('>') else {
            break;
        };
        let placeholder = &rest[pos..pos + close + 1];
        let known = matches!(
            placeholder,
            "<N>" | "<F>" | "<X>" | "<U>" | "<IP>" | "<T>" | "<V>"
        );
        out.push_str(&rest[..pos]);
        if !known {
            out.push_str(placeholder);
        } else {
            match slots.get(i) {
                Some(s) if s.numeric && s.min == s.max => {
                    out.push_str(&format!("<{}:{}>", kind_tag(s.kind), number(s.min)));
                }
                Some(s) if s.numeric => {
                    out.push_str(&format!(
                        "<{}:{}..{}>",
                        kind_tag(s.kind),
                        number(s.min),
                        number(s.max)
                    ));
                }
                _ => out.push_str(placeholder),
            }
            i += 1;
        }
        rest = &rest[pos + close + 1..];
    }
    out.push_str(rest);
    out
}

fn kind_tag(kind: SlotKind) -> &'static str {
    kind.placeholder()
        .trim_start_matches('<')
        .trim_end_matches('>')
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn block(section: Section, n: usize, len: usize) -> Block {
        Block::new(
            section,
            (0..n).map(|i| format!("{i}{}", "x".repeat(len))).collect(),
        )
    }

    #[test]
    fn test_alarm_survives_when_samples_do_not() {
        let out = render(
            vec![
                block(Section::Samples, 40, 60),
                Block::new(Section::Alarm, vec!["ALARM 3 err (exact)".into()]),
                Block::new(Section::Cursor, vec!["cursor v1:g1:o1:l1:e1".into()]),
                block(Section::Top, 40, 60),
            ],
            None,
            600,
        );
        assert!(out.text.contains("ALARM 3 err"));
        assert!(out.text.contains("cursor v1:"));
        assert!(out.truncated);
        // 落としたのは低優先度側であり、必ずフッタに出る
        assert!(out.text.contains("withheld"), "{}", out.text);
        assert!(out.text.contains("SAMPLES"), "{}", out.text);
    }

    #[test]
    fn test_sections_are_emitted_in_priority_order() {
        let out = render(
            vec![
                Block::new(Section::Samples, vec!["s".into()]),
                Block::new(Section::Top, vec!["t".into()]),
                Block::new(Section::Alarm, vec!["a".into()]),
                Block::new(Section::Header, vec!["h".into()]),
            ],
            None,
            2000,
        );
        let body: Vec<&str> = out.text.lines().collect();
        assert_eq!(&body[..4], &["h", "a", "t", "s"]);
    }

    #[test]
    fn test_footer_reports_nothing_withheld_when_everything_fits() {
        let out = render(
            vec![Block::new(Section::Header, vec!["h".into()])],
            None,
            2000,
        );
        assert!(out.text.ends_with("- nothing withheld"));
        assert!(!out.truncated);
    }

    #[test]
    fn test_next_hint_is_carried_in_the_footer() {
        let out = render(
            vec![block(Section::Top, 50, 40)],
            Some("serial_digest(max_chars=4000)"),
            600,
        );
        assert!(out.text.contains("next: serial_digest(max_chars=4000)"));
    }

    #[test]
    fn test_template_with_slots_annotates_numeric_ranges_only() {
        let slot = |kind, numeric, min, max| SlotStat {
            kind,
            numeric,
            min,
            max,
            last: max,
            min_offset: 0,
            max_offset: 0,
        };
        assert_eq!(
            template_with_slots(
                "W: rx overflow q=<N>",
                &[slot(SlotKind::Num, true, 64.0, 64.0)]
            ),
            "W: rx overflow q=<N:64>"
        );
        assert_eq!(
            template_with_slots("T: <F> C", &[slot(SlotKind::Float, true, 21.9, 25.9)]),
            "T: <F:21.9..25.9> C"
        );
        // 数値でないスロットは注釈しない
        assert_eq!(
            template_with_slots("addr=<X>", &[slot(SlotKind::Hex, false, 0.0, 0.0)]),
            "addr=<X>"
        );
        // 山括弧を含むだけのテキストは壊さない
        assert_eq!(template_with_slots("a < b > c", &[]), "a < b > c");
    }

    #[test]
    fn test_number_formatting() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(402_331), "402,331");
        assert_eq!(commas(1_000), "1,000");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2 * 1024 * 1024), "2.00 MB");
        assert_eq!(human_ms(4310), "4.31s");
        assert_eq!(human_ms(90_000), "1m30s");
        assert_eq!(human_rate(0, 0), "n/a");
    }

    proptest! {
        /// どんなブロック構成・どんな予算でも文字数が上限を超えない
        #[test]
        fn prop_render_never_exceeds_budget(
            counts in prop::collection::vec(0usize..30, 0..10),
            len in 0usize..200,
            max_chars in 0usize..4000,
        ) {
            let sections = [
                Section::Header, Section::IndexHealth, Section::Alarm, Section::New,
                Section::Rare, Section::Top, Section::Channels, Section::Timeline,
                Section::Samples, Section::Cursor,
            ];
            let blocks: Vec<Block> = counts
                .iter()
                .enumerate()
                .map(|(i, n)| block(sections[i % sections.len()], *n, len))
                .collect();
            let out = render(blocks, Some("serial_digest(window=\"all\")"), max_chars);
            let effective = max_chars.clamp(MIN_MAX_CHARS, MAX_MAX_CHARS);
            prop_assert!(
                out.text.chars().count() <= effective,
                "{} > {}", out.text.chars().count(), effective
            );
        }

        /// 落とした行があれば必ずフッタに現れる（黙って短くしない）
        #[test]
        fn prop_withholding_is_always_reported(
            n in 0usize..60, len in 0usize..120, max_chars in 500usize..2000,
        ) {
            let out = render(vec![block(Section::Samples, n, len)], None, max_chars);
            if out.truncated {
                prop_assert!(out.text.contains("withheld:"));
            } else {
                prop_assert!(out.text.contains("nothing withheld"));
            }
        }
    }
}
