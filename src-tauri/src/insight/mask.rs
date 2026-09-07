//! 行 → マスク済みテンプレート、および重大度の全バイト走査（純粋関数）
//!
//! ここは索引の中で最も「間違えると静かに嘘をつく」場所なので、IO を一切
//! 持たない純粋関数に切り出して単体 + プロパティ試験で固める（docs/26 §8、
//! docs/22 DEBT-7 の方向）。
//!
//! # マスクの規約
//!
//! 変わりうるトークンだけを型つきのプレースホルダに畳む。**数字で始まる
//! トークンのみ**を対象にするのは、`ch3` や `GPIO12` のような安定した識別子を
//! 変数と誤認しないため。結果として [`mask_line`] は**冪等**である
//! （プレースホルダ `<N>` 等は数字で始まらないので再マスクされない）。
//!
//! # 重大度の規約
//!
//! docs/26 §6.2-3 のとおり、重大度は**取り込む全バイト**を走査する。
//! テンプレート表の追い出しやサンプリングと独立なので、`coverage_pct == 100`
//! である限り件数は正確。数えるのは**出現回数**（1 行に 2 回あれば 2）。

/// テンプレートの最大文字数。これを超える行は末尾を落とす（表の肥大防止）
pub const MAX_TEMPLATE_CHARS: usize = 160;

/// スロット（マスクされた 1 箇所）の種別
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SlotKind {
    /// 整数
    Num,
    /// 小数（指数表記を含む）
    Float,
    /// 16 進（`0x…` または 4 桁以上の 16 進連続）
    Hex,
    /// UUID
    Uuid,
    /// IPv4
    Ip,
    /// 時刻・日付らしきもの
    Time,
    /// 数字で始まるがどれにも当てはまらないもの（表の肥大を防ぐための受け皿）
    Var,
}

impl SlotKind {
    pub fn placeholder(self) -> &'static str {
        match self {
            SlotKind::Num => "<N>",
            SlotKind::Float => "<F>",
            SlotKind::Hex => "<X>",
            SlotKind::Uuid => "<U>",
            SlotKind::Ip => "<IP>",
            SlotKind::Time => "<T>",
            SlotKind::Var => "<V>",
        }
    }
}

/// 1 スロットの観測値
#[derive(Debug, Clone, PartialEq)]
pub struct Slot {
    pub kind: SlotKind,
    /// 数値として解釈できたときのみ `Some`。統計（min/max）はこれだけを使う
    pub num: Option<f64>,
}

/// マスク結果
#[derive(Debug, Clone, PartialEq)]
pub struct Masked {
    /// プレースホルダ入りのテンプレート
    pub template: String,
    /// テンプレート中のプレースホルダと**1 対 1 で対応する**スロット列
    pub slots: Vec<Slot>,
    /// 長すぎて末尾を落とした
    pub truncated: bool,
}

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b':' | b'-' | b'_' | b'+')
}

/// トークンの開始位置か（数字、または符号 + 数字）
fn token_starts_at(line: &[u8], i: usize) -> bool {
    if i > 0 && is_token_byte(line[i - 1]) {
        return false; // 直前から続くトークンの途中
    }
    match line[i] {
        b'0'..=b'9' => true,
        b'-' | b'+' => matches!(line.get(i + 1), Some(b) if b.is_ascii_digit()),
        _ => false,
    }
}

fn all_digits(s: &[u8]) -> bool {
    !s.is_empty() && s.iter().all(u8::is_ascii_digit)
}

fn is_uuid(t: &[u8]) -> bool {
    if t.len() != 36 {
        return false;
    }
    t.iter().enumerate().all(|(i, &b)| match i {
        8 | 13 | 18 | 23 => b == b'-',
        _ => b.is_ascii_hexdigit(),
    })
}

fn is_ipv4(t: &[u8]) -> bool {
    let parts: Vec<&[u8]> = t.split(|&b| b == b'.').collect();
    parts.len() == 4
        && parts.iter().all(|p| {
            all_digits(p)
                && p.len() <= 3
                && std::str::from_utf8(p)
                    .ok()
                    .and_then(|s| s.parse::<u32>().ok())
                    .map(|n| n <= 255)
                    .unwrap_or(false)
        })
}

/// `12:34:56` / `12:34:56.789` / `2026-09-07`
fn is_time_like(t: &[u8]) -> bool {
    if t.contains(&b':') {
        let (head, frac) = match t.iter().position(|&b| b == b'.') {
            Some(p) => (&t[..p], Some(&t[p + 1..])),
            None => (t, None),
        };
        let fields: Vec<&[u8]> = head.split(|&b| b == b':').collect();
        let shaped =
            (2..=3).contains(&fields.len()) && fields.iter().all(|f| all_digits(f) && f.len() <= 2);
        return shaped && frac.map(all_digits).unwrap_or(true);
    }
    // ISO 日付
    let fields: Vec<&[u8]> = t.split(|&b| b == b'-').collect();
    fields.len() == 3
        && fields[0].len() == 4
        && fields[1].len() == 2
        && fields[2].len() == 2
        && fields.iter().all(|f| all_digits(f))
}

fn is_float(t: &[u8]) -> bool {
    let body = match t.first() {
        Some(b'-') | Some(b'+') => &t[1..],
        _ => t,
    };
    let (mantissa, exponent) = match body.iter().position(|b| matches!(b, b'e' | b'E')) {
        Some(p) => (&body[..p], Some(&body[p + 1..])),
        None => (body, None),
    };
    let dot = match mantissa.iter().position(|&b| b == b'.') {
        Some(p) => p,
        None => return false,
    };
    if !all_digits(&mantissa[..dot]) || !all_digits(&mantissa[dot + 1..]) {
        return false;
    }
    match exponent {
        None => true,
        Some(e) => {
            let e = match e.first() {
                Some(b'-') | Some(b'+') => &e[1..],
                _ => e,
            };
            all_digits(e)
        }
    }
}

fn is_int(t: &[u8]) -> bool {
    let body = match t.first() {
        Some(b'-') | Some(b'+') => &t[1..],
        _ => t,
    };
    all_digits(body)
}

/// `0x…` か、16 進英字を含む 4 桁以上の 16 進連続
fn is_hex_literal(t: &[u8]) -> bool {
    if t.len() > 2 && t[0] == b'0' && matches!(t[1], b'x' | b'X') {
        return t[2..].iter().all(u8::is_ascii_hexdigit);
    }
    t.len() >= 4 && t.iter().all(u8::is_ascii_hexdigit) && t.iter().any(|b| b.is_ascii_alphabetic())
}

fn classify(t: &[u8]) -> (SlotKind, Option<f64>) {
    if is_uuid(t) {
        return (SlotKind::Uuid, None);
    }
    if is_ipv4(t) {
        return (SlotKind::Ip, None);
    }
    if is_time_like(t) {
        return (SlotKind::Time, None);
    }
    if is_hex_literal(t) {
        return (SlotKind::Hex, None);
    }
    let text = String::from_utf8_lossy(t);
    if is_int(t) {
        return (SlotKind::Num, text.parse::<f64>().ok());
    }
    if is_float(t) {
        return (SlotKind::Float, text.parse::<f64>().ok());
    }
    (SlotKind::Var, None)
}

/// 1 行をマスクする
///
/// `line` は改行を含まない生バイト列。UTF-8 でなくても panic しない。
///
/// 長さの上限は**バイトではなく文字**で数える。CJK ログを 1 バイト = 1 文字で
/// 概算するとテンプレートが 1/3 の長さで切れ、同じ行が再マスクで別の
/// テンプレートになってしまう（冪等性が壊れる）。
pub fn mask_line(line: &[u8]) -> Masked {
    let line = match line.split_last() {
        Some((b'\r', rest)) => rest,
        _ => line,
    };
    // 以降は妥当な UTF-8 として扱える。トークン構成文字は ASCII のみなので、
    // バイト位置で走査しても多バイト文字の内部を指すことはない。
    let text = String::from_utf8_lossy(line);
    let bytes = text.as_bytes();
    let mut template = String::new();
    let mut chars = 0usize;
    let mut slots = Vec::new();
    let mut truncated = false;
    let mut i = 0usize;

    while i < bytes.len() {
        if token_starts_at(bytes, i) {
            let mut end = i;
            while end < bytes.len() && is_token_byte(bytes[end]) {
                end += 1;
            }
            // 末尾の区切り文字（"12." の "." など）はトークンから外す
            while end > i + 1 && matches!(bytes[end - 1], b'.' | b':' | b'-' | b'_' | b'+') {
                end -= 1;
            }
            let (kind, num) = classify(&bytes[i..end]);
            let placeholder = kind.placeholder();
            let width = placeholder.chars().count();
            if chars + width > MAX_TEMPLATE_CHARS {
                truncated = true;
                break;
            }
            template.push_str(placeholder);
            chars += width;
            slots.push(Slot { kind, num });
            i = end;
        } else {
            let c = match text[i..].chars().next() {
                Some(c) => c,
                None => break,
            };
            if chars + 1 > MAX_TEMPLATE_CHARS {
                truncated = true;
                break;
            }
            template.push(if (c as u32) < 0x20 || c == '\u{7f}' {
                '.'
            } else {
                c
            });
            chars += 1;
            i += c.len_utf8();
        }
    }
    if truncated {
        template.push('\u{2026}');
    }
    Masked {
        template,
        slots,
        truncated,
    }
}

// ============================================================================
// 重大度（全バイト走査）
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Err,
    Warn,
}

/// docs/26 §6.2-3 が列挙する語。増やすときは [`SEVERITY_CARRY`] も見直すこと
const SEVERITY_WORDS: [(&[u8], Severity); 7] = [
    (b"error", Severity::Err),
    (b"fail", Severity::Err),
    (b"fatal", Severity::Err),
    (b"panic", Severity::Err),
    (b"timeout", Severity::Warn),
    (b"overflow", Severity::Warn),
    (b"warn", Severity::Warn),
];

/// 語の最大長 - 1。チャンク境界で語が割れても取り逃がさないための持ち越し量
pub const SEVERITY_CARRY: usize = 7;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeverityCounts {
    pub err: u64,
    pub warn: u64,
}

/// 重大語の出現回数を数える
///
/// `buf` の先頭 `carry` バイトは**前回すでに走査済み**の持ち越し分。そこで
/// 完結するマッチ（終端が `carry` 以下）は前回数えているので数えない。
/// 二重計上も取り逃がしも起きないことを試験で固定している。
pub fn scan_severity(buf: &[u8], carry: usize) -> SeverityCounts {
    let mut counts = SeverityCounts::default();
    for i in 0..buf.len() {
        let first = buf[i].to_ascii_lowercase();
        if !matches!(first, b'e' | b'f' | b'p' | b't' | b'o' | b'w') {
            continue;
        }
        for (word, severity) in SEVERITY_WORDS {
            if word[0] != first {
                continue;
            }
            let end = i + word.len();
            if end > buf.len() || end <= carry {
                continue;
            }
            if buf[i..end].eq_ignore_ascii_case(word) {
                match severity {
                    Severity::Err => counts.err += 1,
                    Severity::Warn => counts.warn += 1,
                }
            }
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn tpl(line: &str) -> String {
        mask_line(line.as_bytes()).template
    }

    #[test]
    fn test_masks_numbers_and_keeps_identifiers() {
        // 数字で始まるトークンだけが畳まれる。ch3 / GPIO12 は識別子として残す
        assert_eq!(tpl("ch3 GPIO12 count=42"), "ch3 GPIO12 count=<N>");
        assert_eq!(tpl("temp=23.4 C"), "temp=<F> C");
        assert_eq!(tpl("delta=-17"), "delta=<N>");
        assert_eq!(tpl("addr=0x48"), "addr=<X>");
        assert_eq!(tpl("id=deadbeef"), "id=deadbeef"); // 文字始まりは識別子扱い
        assert_eq!(tpl("v=1e-3"), "v=<V>"); // 小数点なしの指数は受け皿へ
        assert_eq!(tpl("v=1.5e-3"), "v=<F>");
    }

    #[test]
    fn test_masks_structured_tokens() {
        assert_eq!(tpl("12:34:56.789 boot"), "<T> boot");
        assert_eq!(tpl("2026-09-07 boot"), "<T> boot");
        assert_eq!(tpl("from 192.168.0.11"), "from <IP>");
        assert_eq!(tpl("id 123e4567-e89b-12d3-a456-426614174000"), "id <U>");
        // 255 を超える 4 つ組は IP ではない
        assert_ne!(tpl("v 999.1.1.1"), "v <IP>");
    }

    #[test]
    fn test_trailing_separator_is_not_part_of_the_token() {
        assert_eq!(tpl("count=42."), "count=<N>.");
        assert_eq!(tpl("a 7, b 8;"), "a <N>, b <N>;");
    }

    #[test]
    fn test_control_bytes_become_dots_and_cr_is_stripped() {
        assert_eq!(mask_line(b"a\x00b\r").template, "a.b");
        // ANSI エスケープの "31mred" は英数字が続く 1 トークンなので受け皿へ落ちる。
        // 色コードごとに別テンプレートが生えるのを防ぐ（表の肥大対策）
        assert_eq!(mask_line(b"\x1b[31mred").template, ".[<V>");
    }

    #[test]
    fn test_slots_carry_numeric_values() {
        let m = mask_line(b"temp=23.4 count=7 addr=0x10");
        assert_eq!(m.template, "temp=<F> count=<N> addr=<X>");
        assert_eq!(m.slots.len(), 3);
        assert_eq!(m.slots[0].num, Some(23.4));
        assert_eq!(m.slots[1].num, Some(7.0));
        assert_eq!(m.slots[2].num, None); // 16 進は数値統計に混ぜない
    }

    #[test]
    fn test_long_line_is_truncated_with_marker() {
        let line = "x".repeat(500);
        let m = mask_line(line.as_bytes());
        assert!(m.truncated);
        // 末尾記号 1 文字を除いて上限内
        assert!(m.template.chars().count() <= MAX_TEMPLATE_CHARS + 1);
        assert!(m.template.ends_with('\u{2026}'));
    }

    #[test]
    fn test_severity_counts_occurrences_case_insensitively() {
        let c = scan_severity(b"E: I2C ERROR; warn: retry; Timeout waiting", 0);
        assert_eq!(c.err, 1);
        assert_eq!(c.warn, 2);
        // 部分一致も数える（"errors" は error を含む）
        assert_eq!(scan_severity(b"3 errors", 0).err, 1);
        assert_eq!(scan_severity(b"nothing here", 0), SeverityCounts::default());
    }

    #[test]
    fn test_severity_carry_counts_split_words_exactly_once() {
        // "overflow" を境界で割る。持ち越し 7 バイトで再走査しても二重計上しない
        let whole = b"q overflow now";
        let split_at = 4; // "q ov" | "erflow now"
        let first = scan_severity(&whole[..split_at], 0);
        let carry_start = split_at.saturating_sub(SEVERITY_CARRY);
        let second = scan_severity(&whole[carry_start..], split_at - carry_start);
        assert_eq!(first.warn + second.warn, 1);
        assert_eq!(scan_severity(whole, 0).warn, 1);
    }

    proptest! {
        /// マスクは冪等（テンプレートを再マスクしても変わらない）
        #[test]
        fn prop_mask_is_idempotent(line in prop::collection::vec(any::<u8>(), 0..300)) {
            let once = mask_line(&line);
            let twice = mask_line(once.template.as_bytes());
            prop_assert_eq!(&once.template, &twice.template);
        }

        /// テンプレート中のプレースホルダ数とスロット数が必ず一致する
        #[test]
        fn prop_slots_match_placeholders(line in prop::collection::vec(any::<u8>(), 0..300)) {
            // 入力にプレースホルダそのものが現れると数え方が壊れるので除く
            let line: Vec<u8> = line.into_iter().filter(|b| !matches!(b, b'<' | b'>')).collect();
            let m = mask_line(&line);
            let placeholders: usize = ["<N>", "<F>", "<X>", "<U>", "<IP>", "<T>", "<V>"]
                .iter()
                .map(|p| m.template.matches(p).count())
                .sum();
            prop_assert_eq!(placeholders, m.slots.len());
        }

        /// どんな入力でもテンプレートは上限内（表のメモリが入力に比例しない）
        #[test]
        fn prop_template_is_bounded(line in prop::collection::vec(any::<u8>(), 0..4000)) {
            let m = mask_line(&line);
            prop_assert!(m.template.chars().count() <= MAX_TEMPLATE_CHARS + 1);
        }

        /// 分割して走査しても、まとめて走査した件数と一致する
        #[test]
        fn prop_severity_is_split_invariant(
            data in prop::collection::vec(
                prop::sample::select(vec![b'e', b'r', b'o', b'w', b'a', b'n', b'f', b'l', b' ', b't', b'i', b'm', b'u']),
                0..400),
            cut in 0usize..400,
        ) {
            let whole = scan_severity(&data, 0);
            let cut = cut.min(data.len());
            let first = scan_severity(&data[..cut], 0);
            let carry_start = cut.saturating_sub(SEVERITY_CARRY);
            let second = scan_severity(&data[carry_start..], cut - carry_start);
            prop_assert_eq!(whole.err, first.err + second.err);
            prop_assert_eq!(whole.warn, first.warn + second.warn);
        }
    }
}
