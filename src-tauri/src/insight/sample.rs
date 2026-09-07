//! 生行の抜き取り（純粋関数）
//!
//! 導出情報だけでは足りず「実際の行そのもの」が要る場面のための最小限の窓。
//! docs/26 §4 の `serial_sample`（代表・重複畳み込み・上限付き）に対応する。
//!
//! ここで守る規約:
//!
//! - **端の欠けた行は捨てる。** 途中から始まる行を 1 行として見せると、
//!   エージェントはそれを実データだと信じてしまう。捨てた事実は呼び出し側が
//!   報告する。
//! - **行にはオフセットを付ける。** どの行も `serial_read_range` で確認できる。
//! - **重複は畳むが、件数は正確に残す。** `[x37]` は丸めない。

/// 抜き取った 1 行
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleLine {
    /// ストリーム絶対オフセット（行頭）
    pub offset: u64,
    pub text: String,
    /// 直後に続いた同一行の数を含む出現回数（畳み込み前は 1）
    pub repeats: u32,
    /// 表示上限で末尾を落とした
    pub truncated: bool,
}

/// 1 行の最大文字数
pub const MAX_LINE_CHARS: usize = 200;

fn render_line(raw: &[u8]) -> (String, bool) {
    let raw = match raw.split_last() {
        Some((b'\r', rest)) => rest,
        _ => raw,
    };
    let text = String::from_utf8_lossy(raw);
    let mut out = String::new();
    let mut truncated = false;
    for (i, c) in text.chars().enumerate() {
        if i >= MAX_LINE_CHARS {
            truncated = true;
            break;
        }
        out.push(if (c as u32) < 0x20 || c == '\u{7f}' {
            '.'
        } else {
            c
        });
    }
    (out, truncated)
}

/// バッファから**完全な行だけ**を取り出す
///
/// `trim_head` / `trim_tail` は、その端が読み出し窓で切られている（＝行の
/// 途中かもしれない）ことを呼び出し側が知っている場合に立てる。
pub fn lines_in(buf: &[u8], base: u64, trim_head: bool, trim_tail: bool) -> Vec<SampleLine> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    for i in 0..buf.len() {
        if buf[i] != b'\n' && buf[i] != b'\r' {
            continue;
        }
        if i > start {
            let (text, truncated) = render_line(&buf[start..i]);
            lines.push(SampleLine {
                offset: base + start as u64,
                text,
                repeats: 1,
                truncated,
            });
        }
        start = i + 1;
    }
    // 末尾に改行が無ければ、その行は窓で切られている可能性がある
    if start < buf.len() && !trim_tail {
        let (text, truncated) = render_line(&buf[start..]);
        lines.push(SampleLine {
            offset: base + start as u64,
            text,
            repeats: 1,
            truncated,
        });
    }
    if trim_head && !lines.is_empty() && lines[0].offset == base {
        // 先頭が行頭だったとは限らない
        lines.remove(0);
    }
    lines
}

/// 連続する同一行を 1 行に畳む（件数は正確に保つ）
pub fn collapse(lines: Vec<SampleLine>) -> Vec<SampleLine> {
    let mut out: Vec<SampleLine> = Vec::with_capacity(lines.len());
    for line in lines {
        match out.last_mut() {
            Some(prev) if prev.text == line.text => prev.repeats += 1,
            _ => out.push(line),
        }
    }
    out
}

/// 走査開始位置を n 等分する（`stratified` 用）
///
/// 範囲が狭くて重なる場合は重複を除くので、返る数は `n` 以下。
pub fn stratified_offsets(start: u64, end: u64, n: usize, chunk: u64) -> Vec<u64> {
    if n == 0 || end <= start {
        return Vec::new();
    }
    let span = end - start;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let at = start + (span.saturating_mul(i as u64)) / n as u64;
        let at = at.min(end.saturating_sub(chunk).max(start));
        if out.last() != Some(&at) {
            out.push(at);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lines_in_keeps_offsets() {
        let lines = lines_in(b"aa\nbb\ncc\n", 1000, false, false);
        assert_eq!(
            lines
                .iter()
                .map(|l| (l.offset, l.text.as_str()))
                .collect::<Vec<_>>(),
            vec![(1000, "aa"), (1003, "bb"), (1006, "cc")]
        );
    }

    #[test]
    fn test_partial_edges_are_dropped_when_the_window_cut_them() {
        // 窓の両端で切れている前提の読み出し
        let lines = lines_in(b"lf of a line\nfull\npartial", 500, true, true);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "full");
        assert_eq!(lines[0].offset, 513);
    }

    #[test]
    fn test_crlf_and_cr_only_terminators() {
        assert_eq!(lines_in(b"a\r\nb\r\n", 0, false, false).len(), 2);
        assert_eq!(lines_in(b"a\rb\rc\r", 0, false, false).len(), 3);
    }

    #[test]
    fn test_control_bytes_and_long_lines() {
        let long = vec![b'x'; MAX_LINE_CHARS + 50];
        let lines = lines_in(&long, 0, false, false);
        assert!(lines[0].truncated);
        assert_eq!(lines[0].text.chars().count(), MAX_LINE_CHARS);
        assert_eq!(lines_in(b"a\x00b\n", 0, false, false)[0].text, "a.b");
    }

    #[test]
    fn test_collapse_keeps_exact_counts() {
        let lines = lines_in(b"a\na\na\nb\na\n", 0, false, false);
        let collapsed = collapse(lines);
        assert_eq!(
            collapsed
                .iter()
                .map(|l| (l.text.as_str(), l.repeats))
                .collect::<Vec<_>>(),
            vec![("a", 3), ("b", 1), ("a", 1)]
        );
    }

    #[test]
    fn test_stratified_offsets_span_the_range_without_duplicates() {
        let offsets = stratified_offsets(0, 1000, 5, 100);
        assert_eq!(offsets, vec![0, 200, 400, 600, 800]);
        // 範囲が狭ければ重複を除いて短くなる（水増ししない）
        assert_eq!(stratified_offsets(0, 10, 5, 100), vec![0]);
        assert!(stratified_offsets(5, 5, 3, 10).is_empty());
    }
}
