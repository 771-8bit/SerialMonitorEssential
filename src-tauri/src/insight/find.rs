//! ストリーム全体に対する正規表現検索（索引不要）
//!
//! 「1 MiB 引いてエージェント側で grep する」（約 25 万トークン。しかも
//! 1 MiB を超えると届かない）を、「サーバ側で数えて**件数と少数の
//! ハンドル**を返す」（約 350 トークン）に置き換える。
//!
//! 正確性の要点:
//! - 走査は [`SCAN_WINDOW`] ごとに行い、窓の境界を跨ぐマッチを取り逃がさない
//!   ため [`SCAN_OVERLAP`] だけ重ねて読む。重なりで同じマッチを二重に数えない
//!   よう、報告済みオフセットより後ろだけを採る。
//! - 窓の**末尾で終わる**マッチは打ち切られている可能性があるため、その窓では
//!   採らない。次の窓が重なり分を含めて読み直し、完全な形で拾う。
//! - 読めない領域（`DataStore` の欠落。docs/22 DEBT-5）は**飛ばした事実を
//!   バイト数つきで記録**する。黙って無かったことにしない。
//! - 時間切れで途中終了したときは `complete=false` を返す。呼び出し側は
//!   件数を「正確」ではなく「下限」として提示しなければならない。

use regex::bytes::Regex;

/// 1 回の読み出し窓
pub const SCAN_WINDOW: u32 = 256 * 1024;
/// 窓の重なり。これを超える長さのマッチは境界で取り逃がし得る
pub const SCAN_OVERLAP: u64 = 4096;
/// ヒット 1 件に添える抜粋の最大文字数
pub const EXCERPT_CHARS: usize = 120;

/// 走査対象のバイト列を読む抽象
///
/// 実運用では `serial::DataStore`、試験では `Vec<u8>` を裏に置く。
/// これにより走査ロジックを実ストア無しで細粒度に検証できる（DEBT-7 の方向）。
pub trait ByteSource {
    fn total_bytes(&self) -> u64;
    /// `offset` から最大 `len` バイト読む。欠落なら `Err`
    fn read(&self, offset: u64, len: u32) -> Result<Vec<u8>, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindHit {
    /// ストリーム絶対オフセット（`serial_read_range` にそのまま渡せる）
    pub offset: u64,
    pub len: usize,
    /// マッチを含む行の抜粋（前後は行境界で切る）
    pub excerpt: String,
    /// 抜粋が `EXCERPT_CHARS` で切られた
    pub excerpt_truncated: bool,
}

#[derive(Debug, Clone)]
pub struct FindResult {
    /// 走査できた範囲でのマッチ数。`complete=false` のときは**下限**
    pub matches: u64,
    /// 先頭から `max_hits` 件のヒット
    pub hits: Vec<FindHit>,
    pub first: Option<u64>,
    pub last: Option<u64>,
    /// 要求された走査範囲
    pub range_start: u64,
    pub range_end: u64,
    /// 実際に読めたバイト数（重なりの二重読みは含めない）
    pub bytes_scanned: u64,
    /// 欠落で飛ばしたバイト数
    pub gap_bytes: u64,
    /// 要求範囲を最後まで走査したか
    pub complete: bool,
}

impl FindResult {
    /// 件数を「正確」と言ってよいか
    ///
    /// 途中終了しておらず、欠落も無かったときだけ true。docs/26 の
    /// 「省略による嘘をつかない」を型で担保するためのアクセサ。
    pub fn is_exact(&self) -> bool {
        self.complete && self.gap_bytes == 0
    }
}

pub struct FindOptions<'a> {
    pub regex: &'a Regex,
    pub from: u64,
    pub to: u64,
    pub max_hits: usize,
}

/// マッチを含む行を切り出す
///
/// `buf` 内で改行に挟まれた範囲を取り、長すぎれば `EXCERPT_CHARS` で切る。
fn line_excerpt(buf: &[u8], start: usize, end: usize) -> (String, bool) {
    let line_start = buf[..start]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |p| p + 1);
    let line_end = buf[end..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(buf.len(), |p| end + p);
    let raw = String::from_utf8_lossy(&buf[line_start..line_end]);
    let trimmed = raw.trim_end_matches('\r');
    let mut out = String::new();
    let mut truncated = false;
    for (i, c) in trimmed.chars().enumerate() {
        if i >= EXCERPT_CHARS {
            truncated = true;
            break;
        }
        out.push(c);
    }
    (out, truncated)
}

/// ストリームを走査してマッチを数える
///
/// `expired` が true を返したら速やかに打ち切る（`complete=false` になる）。
/// ブリッジ 1 リクエストのタイムアウトより十分手前で切るために使う。
pub fn find_matches(
    src: &dyn ByteSource,
    opts: &FindOptions,
    expired: &dyn Fn() -> bool,
) -> FindResult {
    let total = src.total_bytes();
    let range_start = opts.from.min(total);
    let range_end = opts.to.min(total);

    let mut result = FindResult {
        matches: 0,
        hits: Vec::new(),
        first: None,
        last: None,
        range_start,
        range_end,
        bytes_scanned: 0,
        gap_bytes: 0,
        complete: true,
    };
    if range_end <= range_start {
        return result;
    }

    // 報告済みの位置。重なりで同じマッチを二度数えないための境界
    let mut reported_up_to = range_start;
    let mut cursor = range_start;

    while cursor < range_end {
        if expired() {
            result.complete = false;
            break;
        }
        let want = (range_end - cursor).min(SCAN_WINDOW as u64) as u32;
        let buf = match src.read(cursor, want) {
            Ok(b) => b,
            Err(_) => {
                // 欠落。窓ごと飛ばし、飛ばした事実を残す
                result.gap_bytes += want as u64;
                cursor += want as u64;
                reported_up_to = reported_up_to.max(cursor);
                continue;
            }
        };
        if buf.is_empty() {
            break;
        }
        let is_last = cursor + buf.len() as u64 >= range_end;

        for m in opts.regex.find_iter(&buf) {
            let abs = cursor + m.start() as u64;
            if abs < reported_up_to {
                continue; // 前の窓で報告済み
            }
            // 窓の末尾で終わるマッチは打ち切られている可能性がある。
            // 次の窓が重なりを含めて読み直すのでそちらに任せる。
            if !is_last && m.end() == buf.len() {
                break;
            }
            result.matches += 1;
            reported_up_to = abs + 1;
            if result.first.is_none() {
                result.first = Some(abs);
            }
            result.last = Some(abs);
            if result.hits.len() < opts.max_hits {
                let (excerpt, excerpt_truncated) = line_excerpt(&buf, m.start(), m.end());
                result.hits.push(FindHit {
                    offset: abs,
                    len: m.end() - m.start(),
                    excerpt,
                    excerpt_truncated,
                });
            }
        }

        result.bytes_scanned += buf.len() as u64;
        if is_last {
            break;
        }
        // 重なりを残して前進。重なり分は次の窓で読み直すので、
        // bytes_scanned が二重計上されないよう前進量で調整する
        let advance = (buf.len() as u64).saturating_sub(SCAN_OVERLAP).max(1);
        result.bytes_scanned -= buf.len() as u64 - advance;
        cursor += advance;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 欠落を再現できる試験用ソース
    struct VecSource {
        data: Vec<u8>,
        /// この範囲に掛かる読み出しは Err にする（DataStore の欠落を模す）
        gap: Option<(u64, u64)>,
    }

    impl VecSource {
        fn new(data: &[u8]) -> Self {
            Self {
                data: data.to_vec(),
                gap: None,
            }
        }
        fn with_gap(data: &[u8], gap: (u64, u64)) -> Self {
            Self {
                data: data.to_vec(),
                gap: Some(gap),
            }
        }
    }

    impl ByteSource for VecSource {
        fn total_bytes(&self) -> u64 {
            self.data.len() as u64
        }
        fn read(&self, offset: u64, len: u32) -> Result<Vec<u8>, String> {
            let end = (offset + len as u64).min(self.data.len() as u64);
            if let Some((gs, ge)) = self.gap {
                if offset < ge && end > gs {
                    return Err("gap".to_string());
                }
            }
            if offset >= self.data.len() as u64 {
                return Ok(Vec::new());
            }
            Ok(self.data[offset as usize..end as usize].to_vec())
        }
    }

    fn find(data: &[u8], pattern: &str, max_hits: usize) -> FindResult {
        let re = Regex::new(pattern).unwrap();
        let src = VecSource::new(data);
        find_matches(
            &src,
            &FindOptions {
                regex: &re,
                from: 0,
                to: u64::MAX,
                max_hits,
            },
            &|| false,
        )
    }

    #[test]
    fn test_counts_are_exact_and_offsets_absolute() {
        let data = b"ok\nE: bad\nok\nE: worse\nok\n";
        let r = find(data, "E: ", 10);
        assert_eq!(r.matches, 2);
        assert!(r.is_exact());
        assert_eq!(r.first, Some(3));
        assert_eq!(r.last, Some(13));
        assert_eq!(r.hits[0].offset, 3);
        assert_eq!(r.hits[0].excerpt, "E: bad");
        assert_eq!(r.hits[1].excerpt, "E: worse");
    }

    #[test]
    fn test_no_match_is_an_exact_zero() {
        let r = find(b"all fine\n", "PANIC", 10);
        assert_eq!(r.matches, 0);
        assert!(r.is_exact(), "a clean scan must be reportable as exact 0");
        assert_eq!(r.first, None);
    }

    #[test]
    fn test_match_spanning_a_window_boundary_is_found_once() {
        // 窓境界 (SCAN_WINDOW) を跨ぐ位置にちょうどマッチを置く。
        // 重なりが無ければ取り逃がし、重なりの扱いを誤れば二重に数える。
        let needle = b"NEEDLE-ACROSS";
        let boundary = SCAN_WINDOW as usize;
        let mut data = vec![b'.'; boundary * 2];
        let at = boundary - needle.len() / 2;
        data[at..at + needle.len()].copy_from_slice(needle);
        // 行にしておく（抜粋の検証のため）
        data[at - 1] = b'\n';
        data[at + needle.len()] = b'\n';

        let r = find(&data, "NEEDLE-ACROSS", 10);
        assert_eq!(r.matches, 1, "must be counted exactly once");
        assert_eq!(r.first, Some(at as u64));
        assert_eq!(r.hits[0].excerpt, "NEEDLE-ACROSS");
    }

    #[test]
    fn test_repeated_matches_across_many_windows_are_counted_once_each() {
        // 各窓に複数、かつ重なり領域にも入る配置
        let mut data = Vec::new();
        let mut expected = 0u64;
        while data.len() < SCAN_WINDOW as usize * 3 {
            data.extend_from_slice(b"tick\nERR x\n");
            expected += 1;
        }
        let r = find(&data, "ERR x", 5);
        assert_eq!(r.matches, expected);
        assert!(r.is_exact());
        assert_eq!(r.hits.len(), 5, "hits are capped, the count is not");
    }

    #[test]
    fn test_gap_is_reported_not_hidden() {
        let mut data = Vec::new();
        while data.len() < SCAN_WINDOW as usize * 2 {
            data.extend_from_slice(b"ERR here\n");
        }
        let src = VecSource::with_gap(&data, (0, SCAN_WINDOW as u64));
        let re = Regex::new("ERR").unwrap();
        let r = find_matches(
            &src,
            &FindOptions {
                regex: &re,
                from: 0,
                to: u64::MAX,
                max_hits: 3,
            },
            &|| false,
        );
        assert!(r.gap_bytes > 0, "the unreadable window must be reported");
        assert!(
            !r.is_exact(),
            "a scan that skipped bytes must never claim an exact count"
        );
        assert!(r.matches > 0, "the readable part is still scanned");
    }

    #[test]
    fn test_timeout_marks_the_count_as_incomplete() {
        let mut data = Vec::new();
        while data.len() < SCAN_WINDOW as usize * 4 {
            data.extend_from_slice(b"ERR\n");
        }
        let src = VecSource::new(&data);
        let re = Regex::new("ERR").unwrap();
        let calls = std::cell::Cell::new(0u32);
        let r = find_matches(
            &src,
            &FindOptions {
                regex: &re,
                from: 0,
                to: u64::MAX,
                max_hits: 3,
            },
            &|| {
                calls.set(calls.get() + 1);
                calls.get() > 2 // 2 窓ぶんだけ走査させる
            },
        );
        assert!(!r.complete);
        assert!(!r.is_exact());
        assert!(r.matches > 0);
        assert!(
            r.bytes_scanned < data.len() as u64,
            "an early stop must not claim full coverage"
        );
    }

    #[test]
    fn test_range_is_honoured_and_clamped() {
        let data = b"ERR a\nERR b\nERR c\n";
        let re = Regex::new("ERR").unwrap();
        let src = VecSource::new(data);
        let r = find_matches(
            &src,
            &FindOptions {
                regex: &re,
                from: 6,
                to: 12,
                max_hits: 10,
            },
            &|| false,
        );
        assert_eq!(r.matches, 1);
        assert_eq!(r.first, Some(6));
        assert_eq!(r.range_end, 12);

        // to がストリーム長を超えても total で頭打ち
        let r = find_matches(
            &src,
            &FindOptions {
                regex: &re,
                from: 0,
                to: u64::MAX,
                max_hits: 10,
            },
            &|| false,
        );
        assert_eq!(r.range_end, data.len() as u64);
    }

    #[test]
    fn test_excerpt_is_clipped_to_its_line_and_budget() {
        let long = format!("E: {}\nnext line\n", "x".repeat(500));
        let r = find(long.as_bytes(), "E: ", 1);
        assert!(r.hits[0].excerpt_truncated);
        assert_eq!(r.hits[0].excerpt.chars().count(), EXCERPT_CHARS);
        assert!(
            !r.hits[0].excerpt.contains("next line"),
            "the excerpt must not run past the end of its line"
        );
    }
}
