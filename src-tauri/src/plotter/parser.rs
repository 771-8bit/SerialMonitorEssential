// Plotter Parser - Parse serial data into plottable values
//
// Supports:
// - Single values: `123.45\r\n`
// - CSV format: `10,20,30\r\n`
// - Tab/space separated: `10\t20\t30\r\n`
// - Labeled values: `temp:25.5,humidity:60\r\n`, `V=30.58 I=0.8\r\n`,
//   `temp = 25 hum: 60\r\n`, `temp 25.5 hum 60\r\n` (see `parse_line`)
// - Header detection: first line with all non-numeric values
// - State values: `state:RUNNING\r\n`

use std::collections::HashMap;

/// Parsed data point from a single line
#[derive(Debug, Clone)]
pub struct ParsedDataPoint {
    /// Timestamp in milliseconds since reception start
    pub timestamp_ms: u64,
    /// Channel data (label -> value)
    pub channels: HashMap<String, ChannelValue>,
    /// Channel order (column order from left to right)
    pub channel_order: Vec<String>,
}

/// Channel value - either numeric or state
#[derive(Debug, Clone, PartialEq)]
pub enum ChannelValue {
    /// Numeric value for line chart
    Numeric(f64),
    /// State value for state timeline
    State(String),
}

/// Parser state and configuration
#[derive(Debug)]
pub struct PlotterParser {
    /// Channel labels (from header or auto-generated)
    labels: Vec<String>,
    /// Whether header has been detected
    header_detected: bool,
    /// Whether any numeric data has been parsed yet
    /// (used to reject header-like lines appearing mid-stream)
    saw_numeric_data: bool,
    /// Buffer for incomplete lines (raw bytes; converted to text per complete
    /// line so multi-byte UTF-8 characters split across reads stay intact)
    line_buffer: Vec<u8>,
    /// Set after an oversized line was dropped: everything up to the next line
    /// ending belongs to that line and must be discarded, not parsed.
    discard_until_newline: bool,
    /// Timestamp of the last parsed line (used to detect device-reset gaps)
    last_line_ts: u64,
    /// The next line may have started before the bytes we have (we joined the
    /// stream mid-line: a connect or a resumed reception). It is used only to
    /// pick up a CSV header and never becomes a data point (docs/27 PF-2).
    untrusted_first_line: bool,
}

impl Default for PlotterParser {
    fn default() -> Self {
        Self::new()
    }
}

impl PlotterParser {
    pub fn new() -> Self {
        Self {
            labels: Vec::new(),
            header_detected: false,
            saw_numeric_data: false,
            line_buffer: Vec::new(),
            discard_until_newline: false,
            last_line_ts: 0,
            untrusted_first_line: false,
        }
    }

    /// The following bytes start a new receive segment whose first line may be
    /// cut (connecting to a device that is already streaming, or reception
    /// resumed after an automatic reconnect). Drop any partial line carried
    /// over from before, and treat the next line as untrusted: it can still be
    /// adopted as a CSV header, but it never produces a data point. A cut data
    /// line like "39.05,cos:" joined to the next line would otherwise create a
    /// phantom channel that stays for the whole session.
    pub fn begin_untrusted_segment(&mut self) {
        self.line_buffer.clear();
        self.discard_until_newline = false;
        self.untrusted_first_line = true;
    }

    /// Reset parser state (for new session)
    pub fn reset(&mut self) {
        self.labels.clear();
        self.header_detected = false;
        self.saw_numeric_data = false;
        self.line_buffer.clear();
        self.discard_until_newline = false;
        self.last_line_ts = 0;
        self.untrusted_first_line = false;
    }

    /// Parse incoming bytes into data points
    ///
    /// Returns a vector of parsed data points. Each complete line produces one data point.
    /// Incomplete lines are buffered for the next call.
    pub fn parse(&mut self, data: &[u8], timestamp_ms: u64) -> Vec<ParsedDataPoint> {
        let mut results = Vec::new();

        self.line_buffer.extend_from_slice(data);

        // If a previous oversized line was dropped, keep discarding until the
        // next line ending so its tail can't be parsed as a bogus data point.
        if self.discard_until_newline {
            match self
                .line_buffer
                .iter()
                .position(|&b| b == b'\r' || b == b'\n')
            {
                Some(pos) => {
                    self.line_buffer.drain(..pos);
                    self.discard_until_newline = false;
                }
                None => {
                    self.line_buffer.clear();
                    return Vec::new();
                }
            }
        }

        // Scan for complete lines at the byte level (single pass, no cloning
        // of the whole buffer per line)
        let mut line_start = 0usize;
        let mut i = 0usize;
        let buf_len = self.line_buffer.len();

        while i < buf_len {
            let b = self.line_buffer[i];
            if b == b'\r' || b == b'\n' {
                if self.untrusted_first_line {
                    // The first line of a segment: header detection only.
                    // Released at the first line ending, even an empty line.
                    if i > line_start {
                        let line =
                            String::from_utf8_lossy(&self.line_buffer[line_start..i]).into_owned();
                        self.adopt_header_only(&line);
                    }
                    self.untrusted_first_line = false;
                } else if i > line_start {
                    // Convert only the complete line to text (lossy for non-UTF8)
                    let line =
                        String::from_utf8_lossy(&self.line_buffer[line_start..i]).into_owned();
                    if !line.trim().is_empty() {
                        if let Some(data_point) = self.parse_line(&line, timestamp_ms) {
                            results.push(data_point);
                        }
                    }
                }
                // Skip the newline (CRLF counts as one)
                if b == b'\r' && i + 1 < buf_len && self.line_buffer[i + 1] == b'\n' {
                    i += 2;
                } else {
                    i += 1;
                }
                line_start = i;
            } else {
                i += 1;
            }
        }

        // Keep the incomplete tail for the next call
        self.line_buffer.drain(..line_start);

        // Safety cap: a stream with no newlines (e.g. binary data) must not
        // grow the buffer without bound. Real plotter lines are far shorter.
        const MAX_LINE_BUFFER: usize = 64 * 1024;
        if self.line_buffer.len() > MAX_LINE_BUFFER {
            log::warn!(
                "[PlotterParser] Dropping {} buffered bytes with no line ending (binary data?)",
                self.line_buffer.len()
            );
            self.line_buffer.clear();
            // The rest of this oversized line is still coming: discard it too,
            // otherwise its tail would be parsed as a fresh (bogus) line.
            self.discard_until_newline = true;
            // That oversized line was the untrusted one; the next is trusted
            self.untrusted_first_line = false;
        }

        results
    }

    /// Adopt `line` as the CSV header if it is one; otherwise ignore it with no
    /// side effects (an untrusted, possibly cut line must not count as data)
    fn adopt_header_only(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() || self.header_detected || self.saw_numeric_data {
            return;
        }
        let separator = self.detect_separator(line);
        let parts: Vec<&str> = if separator == ' ' {
            line.split_whitespace().collect()
        } else {
            line.split(separator).map(|s| s.trim()).collect()
        };
        if self.is_header_line(&parts) {
            self.labels = parts.iter().map(|s| s.to_string()).collect();
            self.header_detected = true;
        }
    }

    /// Parse a single line into a data point
    fn parse_line(&mut self, line: &str, timestamp_ms: u64) -> Option<ParsedDataPoint> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }

        // Detect separator for THIS line (comma > tab > space priority).
        // Re-detecting per line prevents a stray non-CSV line (e.g. a boot
        // banner like "Boot OK") from permanently locking in the wrong
        // separator for all subsequent data.
        let separator = self.detect_separator(line);
        let whitespace_mode = separator == ' ';

        // For space separation, collapse runs of spaces (printf-style aligned
        // output) instead of producing empty parts that shift column indices.
        // A joiner with spaces around it ("temp = 25", "temp: 25") is closed up
        // first so the pair stays one token.
        let closed_up;
        let parts: Vec<&str> = if whitespace_mode {
            closed_up = close_up_joiners(line);
            closed_up.split_whitespace().collect()
        } else {
            line.split(separator).map(|s| s.trim()).collect()
        };

        // Device-reset heuristic: a silence of >=2s (bootloader / manual reset)
        // means the next lines may start a fresh stream, so re-arm header
        // detection that backlog numeric lines would otherwise have latched off.
        // Only while no header has been adopted yet - an established header and
        // its labels stay in effect.
        if !self.header_detected && timestamp_ms > self.last_line_ts + 2000 {
            self.saw_numeric_data = false;
        }
        self.last_line_ts = timestamp_ms;

        // Check if this is a header line (all non-numeric).
        // Only before any numeric data has flowed: header-like lines appearing
        // mid-stream (e.g. "ERROR,WARN") must not silently rename all channels.
        if !self.header_detected && !self.saw_numeric_data && self.is_header_line(&parts) {
            self.labels = parts.iter().map(|s| s.to_string()).collect();
            self.header_detected = true;
            return None; // Don't return data point for header
        }

        // Classify every field: a named pair ("name=value" / "name:value") or a
        // bare token. A pair whose value is empty or unusable ("key=",
        // "I=0.825/0.797") is dropped outright: it must not fall back to a
        // positional State channel such as "ch3 = key=".
        let mut fields: Vec<Field> = parts
            .iter()
            .map(|part| match split_pair(part) {
                Some((name, value)) => match classify_value(value) {
                    Some(v) => Field::Pair(name.to_string(), v),
                    None => Field::Dropped,
                },
                None => Field::Bare(part),
            })
            .collect();

        // Space as the joiner ("temp 25.5 hum 60"): only in whitespace mode,
        // only without an adopted header (its labels name the columns), and only
        // when EVERY bare number directly follows a bare word - otherwise the line
        // is positional as before ("RUNNING 25.5 60", "25.5 60 RUNNING").
        if whitespace_mode && !self.header_detected {
            pair_words_with_numbers(&mut fields);
        }

        // In a whitespace line with named pairs, leftover bare words are prose or
        // decoration ("[Mon", "occurred"), not a column of states.
        let has_pairs = fields.iter().any(|f| matches!(f, Field::Pair(..)));
        let drop_bare_words = whitespace_mode && has_pairs;

        let mut channels = HashMap::new();
        let mut channel_order = Vec::new();

        for (i, field) in fields.into_iter().enumerate() {
            match field {
                Field::Pair(label, value) => {
                    channels.insert(label.clone(), value);
                    channel_order.push(label);
                }
                Field::Bare(part) => {
                    let Some(value) = classify_value(part) else {
                        continue;
                    };
                    if drop_bare_words && !matches!(value, ChannelValue::Numeric(_)) {
                        continue;
                    }
                    // Auto-generate label
                    let label = if i < self.labels.len() {
                        self.labels[i].clone()
                    } else {
                        format!("ch{}", i)
                    };
                    channels.insert(label.clone(), value);
                    channel_order.push(label);
                }
                Field::Dropped => {}
            }
        }

        if channels.is_empty() {
            return None;
        }

        if channels
            .values()
            .any(|v| matches!(v, ChannelValue::Numeric(_)))
        {
            self.saw_numeric_data = true;
        }

        Some(ParsedDataPoint {
            timestamp_ms,
            channels,
            channel_order,
        })
    }

    /// Detect the separator used in the line
    fn detect_separator(&self, line: &str) -> char {
        // Priority: comma > tab > space
        if line.contains(',') {
            ','
        } else if line.contains('\t') {
            '\t'
        } else {
            ' '
        }
    }

    /// Check if a line is a header (all values are non-numeric and not labeled)
    /// Only multi-column lines can be headers (single values are data)
    fn is_header_line(&self, parts: &[&str]) -> bool {
        // Single-value lines are never headers
        if parts.len() < 2 {
            return false;
        }

        // If any part contains ':' or '=', it's a labeled value line, not a header
        if parts.iter().any(|part| part.contains([':', '='])) {
            return false;
        }

        // All parts must be non-numeric and non-empty
        parts.iter().all(|part| {
            let trimmed = part.trim();
            !trimmed.is_empty() && trimmed.parse::<f64>().is_err()
        })
    }
}

/// One field of a line, before labels are assigned
enum Field<'a> {
    /// "name=value" / "name:value" (or a word followed by a number)
    Pair(String, ChannelValue),
    /// A token without a joiner (labelled by its column)
    Bare(&'a str),
    /// A pair whose value is empty or unusable
    Dropped,
}

/// Whether `s` can be a channel name: it starts with a letter (any script, so
/// "温度" works) or '_'. This keeps a time of day ("12:34:56") or "3:5" from
/// being read as name and value.
fn is_name(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
}

/// Split "name=value" / "name:value" at the first '=' or ':'. None when there is
/// no joiner or the part before it is not a name.
fn split_pair(field: &str) -> Option<(&str, &str)> {
    let at = field.find(['=', ':'])?;
    let name = field[..at].trim();
    is_name(name).then(|| (name, field[at + 1..].trim()))
}

/// Remove the whitespace around '=' and ':' so "temp = 25" and "temp: 25"
/// survive the whitespace split as one token.
fn close_up_joiners(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut after_joiner = false;
    for c in line.chars() {
        if c == '=' || c == ':' {
            let kept = out.trim_end().len();
            out.truncate(kept);
            out.push(c);
            after_joiner = true;
        } else if after_joiner && c.is_whitespace() {
            // skip
        } else {
            out.push(c);
            after_joiner = false;
        }
    }
    out
}

/// Pair "word number" in place when every bare number in the line directly
/// follows a bare word (see `parse_line`). Leaves the fields unchanged otherwise.
fn pair_words_with_numbers(fields: &mut Vec<Field>) {
    let is_number = |f: &Field| matches!(f, Field::Bare(t) if matches!(classify_value(t), Some(ChannelValue::Numeric(_))));
    let is_word = |f: &Field| matches!(f, Field::Bare(t) if is_name(t));

    let numbers: Vec<usize> = (0..fields.len())
        .filter(|&i| is_number(&fields[i]))
        .collect();
    if numbers.is_empty() || numbers.iter().any(|&i| i == 0 || !is_word(&fields[i - 1])) {
        return;
    }
    for &i in numbers.iter().rev() {
        let (Field::Bare(word), Field::Bare(num)) = (&fields[i - 1], &fields[i]) else {
            continue;
        };
        let Some(value) = classify_value(num) else {
            continue;
        };
        fields[i - 1] = Field::Pair(word.to_string(), value);
        fields.remove(i);
    }
}

/// Longest unit suffix accepted after a number ("ms", "V", "%", "dB", "]")
const MAX_UNIT_CHARS: usize = 4;

/// Interpret a value:
/// - a finite number → Numeric
/// - starting with a letter ("RUN", "MODE2", "NaN") → State
/// - a number followed by a short unit without digits ("11ms", "3.3V", "100%",
///   "2916094]") → Numeric, unit dropped
/// - anything else that is neither ("0.825/0.797", "1/1", "12:34:56", "-inf")
///   → None: as a State it would be a new value on every line, filling the
///   state timeline with noise
fn classify_value(s: &str) -> Option<ChannelValue> {
    let s = s.trim();
    let first = s.chars().next()?;
    if let Ok(num) = s.parse::<f64>() {
        if num.is_finite() {
            return Some(ChannelValue::Numeric(num));
        }
    }
    // A joiner left inside a value ("accel:0.049", "a=b") means the field
    // was not one name/value pair: neither a state nor a number
    if s.contains(['=', ':']) {
        return None;
    }
    if first.is_alphabetic() || first == '_' {
        return Some(ChannelValue::State(s.to_string()));
    }
    // Number + unit: try the split points from the longest number down
    let unit_ok = |u: &str| {
        u.chars().count() <= MAX_UNIT_CHARS
            && !u
                .chars()
                .any(|c| c.is_ascii_digit() || c == '=' || c == ':')
    };
    for (at, _) in s.char_indices().rev().filter(|&(i, _)| i > 0) {
        let (num, unit) = s.split_at(at);
        if !unit_ok(unit) {
            // A longer unit only gets longer: no shorter number can work
            if unit.chars().count() > MAX_UNIT_CHARS {
                break;
            }
            continue;
        }
        if let Ok(v) = num.parse::<f64>() {
            if v.is_finite() {
                return Some(ChannelValue::Numeric(v));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_single_value() {
        let mut parser = PlotterParser::new();
        let results = parser.parse(b"123.45\r\n", 1000);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].timestamp_ms, 1000);
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::Numeric(123.45))
        );
    }

    #[test]
    fn test_parse_csv() {
        let mut parser = PlotterParser::new();
        let results = parser.parse(b"10,20,30\r\n", 1000);

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::Numeric(10.0))
        );
        assert_eq!(
            results[0].channels.get("ch1"),
            Some(&ChannelValue::Numeric(20.0))
        );
        assert_eq!(
            results[0].channels.get("ch2"),
            Some(&ChannelValue::Numeric(30.0))
        );
    }

    #[test]
    fn test_parse_tab_separated() {
        let mut parser = PlotterParser::new();
        let results = parser.parse(b"10\t20\t30\r\n", 1000);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].channels.len(), 3);
    }

    #[test]
    fn test_parse_labeled_value() {
        let mut parser = PlotterParser::new();
        let results = parser.parse(b"temp:25.5\r\n", 1000);

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].channels.get("temp"),
            Some(&ChannelValue::Numeric(25.5))
        );
    }

    #[test]
    fn test_parse_multiple_labeled_values() {
        let mut parser = PlotterParser::new();
        let results = parser.parse(b"temp:25.5,humidity:60\r\n", 1000);

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].channels.get("temp"),
            Some(&ChannelValue::Numeric(25.5))
        );
        assert_eq!(
            results[0].channels.get("humidity"),
            Some(&ChannelValue::Numeric(60.0))
        );
    }

    #[test]
    fn test_parse_header_detection() {
        let mut parser = PlotterParser::new();

        // First line is header
        let results = parser.parse(b"a,b,c\r\n", 1000);
        assert_eq!(results.len(), 0); // Header doesn't produce data point

        // Second line uses header labels
        let results = parser.parse(b"1,2,3\r\n", 2000);
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].channels.get("a"),
            Some(&ChannelValue::Numeric(1.0))
        );
        assert_eq!(
            results[0].channels.get("b"),
            Some(&ChannelValue::Numeric(2.0))
        );
        assert_eq!(
            results[0].channels.get("c"),
            Some(&ChannelValue::Numeric(3.0))
        );
    }

    #[test]
    fn test_parse_error_skip() {
        let mut parser = PlotterParser::new();

        // Single non-numeric value is treated as state, not header
        // Then parse numeric line
        let results = parser.parse(b"abc\r\n123\r\n", 1000);

        // "abc" is state value on ch0, "123" is numeric on ch0
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::State("abc".to_string()))
        );
        assert_eq!(
            results[1].channels.get("ch0"),
            Some(&ChannelValue::Numeric(123.0))
        );
    }

    #[test]
    fn test_parse_state_value() {
        let mut parser = PlotterParser::new();
        let results = parser.parse(b"state:RUNNING\r\n", 1000);

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].channels.get("state"),
            Some(&ChannelValue::State("RUNNING".to_string()))
        );
    }

    #[test]
    fn test_parse_mixed_state_and_numeric() {
        let mut parser = PlotterParser::new();
        let results = parser.parse(b"temp:25.5,state:ON\r\n", 1000);

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].channels.get("temp"),
            Some(&ChannelValue::Numeric(25.5))
        );
        assert_eq!(
            results[0].channels.get("state"),
            Some(&ChannelValue::State("ON".to_string()))
        );
    }

    #[test]
    fn test_parse_incomplete_line() {
        let mut parser = PlotterParser::new();

        // Partial data
        let results = parser.parse(b"123", 1000);
        assert_eq!(results.len(), 0);

        // Complete with newline
        let results = parser.parse(b".45\r\n", 2000);
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::Numeric(123.45))
        );
    }

    #[test]
    fn test_parse_lf_only() {
        let mut parser = PlotterParser::new();
        let results = parser.parse(b"123.45\n", 1000);

        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_parse_cr_only() {
        let mut parser = PlotterParser::new();
        let results = parser.parse(b"123.45\r", 1000);

        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_parse_nan_excluded() {
        let mut parser = PlotterParser::new();
        let results = parser.parse(b"NaN\r\n", 1000);

        // NaN should be treated as state, not numeric
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::State("NaN".to_string()))
        );
    }

    #[test]
    fn test_parse_negative_numbers() {
        let mut parser = PlotterParser::new();
        let results = parser.parse(b"-123.45\r\n", 1000);

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::Numeric(-123.45))
        );
    }

    #[test]
    fn test_banner_line_does_not_lock_separator() {
        // A non-CSV banner line (e.g. firmware boot message) must not lock in
        // the wrong separator for subsequent CSV data.
        let mut parser = PlotterParser::new();
        let results = parser.parse(b"READY\r\n10,20,30\r\n", 1000);

        assert_eq!(results.len(), 2);
        // Banner becomes a state value on ch0
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::State("READY".to_string()))
        );
        // CSV line must still parse as 3 numeric channels
        assert_eq!(
            results[1].channels.get("ch0"),
            Some(&ChannelValue::Numeric(10.0))
        );
        assert_eq!(
            results[1].channels.get("ch1"),
            Some(&ChannelValue::Numeric(20.0))
        );
        assert_eq!(
            results[1].channels.get("ch2"),
            Some(&ChannelValue::Numeric(30.0))
        );
    }

    #[test]
    fn test_separator_can_change_between_lines() {
        // Space-separated line followed by comma-separated line: each line
        // is interpreted with its own separator.
        let mut parser = PlotterParser::new();
        let results = parser.parse(b"1 2\r\n3,4\r\n", 1000);

        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::Numeric(1.0))
        );
        assert_eq!(
            results[0].channels.get("ch1"),
            Some(&ChannelValue::Numeric(2.0))
        );
        assert_eq!(
            results[1].channels.get("ch0"),
            Some(&ChannelValue::Numeric(3.0))
        );
        assert_eq!(
            results[1].channels.get("ch1"),
            Some(&ChannelValue::Numeric(4.0))
        );
    }

    #[test]
    fn test_reset() {
        let mut parser = PlotterParser::new();
        parser.parse(b"a,b,c\r\n1,2,3\r\n", 1000);

        parser.reset();

        // After reset, should behave like new parser
        let results = parser.parse(b"x,y\r\n", 2000);
        assert_eq!(results.len(), 0); // Header again

        let results = parser.parse(b"4,5\r\n", 3000);
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].channels.get("x"),
            Some(&ChannelValue::Numeric(4.0))
        );
    }

    // ================================================================
    // Device-reset gap: header re-arm (parse_line)
    // ================================================================
    //
    // `parse_line` re-arms header detection when a line arrives more than
    // 2000 ms after the PREVIOUS line, and only while no header has been
    // adopted yet:
    //
    //     if !self.header_detected && timestamp_ms > self.last_line_ts + 2000 {
    //         self.saw_numeric_data = false;
    //     }
    //     self.last_line_ts = timestamp_ms;
    //
    // Each of the four conditions below (the `!`, the `&&`, the strict `>`,
    // and the `+ 2000`) is pinned by at least one test.

    /// Header-like line seen 1999 ms after data: the gap is BELOW the
    /// threshold, so the line is data (state values), not a header.
    ///
    /// Pins the `&&`: re-arming on `!header_detected` alone would adopt it.
    #[test]
    fn test_header_not_readopted_within_2s_gap() {
        let mut parser = PlotterParser::new();

        // Numeric data latches saw_numeric_data.
        let results = parser.parse(b"1,2\n", 1000);
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::Numeric(1.0))
        );

        // Gap of 1999 ms (< 2000): NOT a device reset.
        let results = parser.parse(b"temp,humidity\n", 2999);
        assert_eq!(results.len(), 1, "header-like line must stay data");
        assert_eq!(results[0].channels.len(), 2);
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::State("temp".to_string()))
        );
        assert_eq!(
            results[0].channels.get("ch1"),
            Some(&ChannelValue::State("humidity".to_string()))
        );
        assert_eq!(results[0].channel_order, vec!["ch0", "ch1"]);
    }

    /// Header-like line seen 2001 ms after data: the gap exceeds the
    /// threshold, header detection is re-armed and the line IS adopted.
    ///
    /// Pins the leading `!`, the strict `>` (an `==`/`<` there would miss
    /// 2001) and the `+` in `last_line_ts + 2000` (a `*` would need
    /// 1000 * 2000 ms of silence).
    #[test]
    fn test_header_readopted_after_2s_gap() {
        let mut parser = PlotterParser::new();

        let results = parser.parse(b"1,2\n", 1000);
        assert_eq!(results.len(), 1);

        // Gap of 2001 ms (> 2000): device reset, header detection re-arms.
        let results = parser.parse(b"temp,humidity\n", 3001);
        assert_eq!(results.len(), 0, "line must be adopted as a header");

        // The adopted labels now name the columns.
        let results = parser.parse(b"1,2\n", 3100);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].channels.len(), 2);
        assert_eq!(
            results[0].channels.get("temp"),
            Some(&ChannelValue::Numeric(1.0))
        );
        assert_eq!(
            results[0].channels.get("humidity"),
            Some(&ChannelValue::Numeric(2.0))
        );
        assert_eq!(results[0].channel_order, vec!["temp", "humidity"]);
    }

    /// BVA on the gap threshold: exactly 2000 ms does NOT re-arm
    /// (the comparison is strictly greater-than).
    #[test]
    fn test_header_rearm_gap_boundary_is_strict() {
        let mut parser = PlotterParser::new();

        let results = parser.parse(b"1,2\n", 1000);
        assert_eq!(results.len(), 1);

        // Gap of EXACTLY 2000 ms: still below the re-arm condition.
        let results = parser.parse(b"temp,humidity\n", 3000);
        assert_eq!(
            results.len(),
            1,
            "a gap of exactly 2000 ms must not re-arm header detection"
        );
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::State("temp".to_string()))
        );
        assert_eq!(
            results[0].channels.get("ch1"),
            Some(&ChannelValue::State("humidity".to_string()))
        );

        // ...and one millisecond past the boundary (measured from the line
        // just parsed at t=3000) it does re-arm.
        let results = parser.parse(b"temp,humidity\n", 5001);
        assert_eq!(results.len(), 0, "2001 ms of silence must re-arm");
    }

    /// The gap is measured from the LAST line of any kind, not from the last
    /// numeric line: a state/banner line in between keeps the tracker fresh.
    #[test]
    fn test_last_line_ts_tracks_every_line_not_only_numeric() {
        let mut parser = PlotterParser::new();

        // Numeric data at t=1000.
        let results = parser.parse(b"1,2\n", 1000);
        assert_eq!(results.len(), 1);

        // Non-numeric single-token line at t=2500 (gap 1500: no re-arm).
        let results = parser.parse(b"boot\n", 2500);
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::State("boot".to_string()))
        );

        // t=4300 is 3300 ms after the last NUMERIC line but only 1800 ms
        // after the last line, so header detection must stay disarmed.
        let results = parser.parse(b"temp,humidity\n", 4300);
        assert_eq!(
            results.len(),
            1,
            "gap must be measured from the last line, not the last numeric line"
        );
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::State("temp".to_string()))
        );
        assert_eq!(
            results[0].channels.get("ch1"),
            Some(&ChannelValue::State("humidity".to_string()))
        );
    }

    /// Once a header has been adopted it survives any silence: the re-arm is
    /// guarded by `!header_detected` and never renames established channels.
    #[test]
    fn test_adopted_header_survives_long_gap() {
        let mut parser = PlotterParser::new();

        let results = parser.parse(b"a,b\n", 1000);
        assert_eq!(results.len(), 0, "first line is the header");

        let results = parser.parse(b"1,2\n", 2000);
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].channels.get("a"),
            Some(&ChannelValue::Numeric(1.0))
        );

        // 98 seconds of silence, then another header-like line: the labels
        // stay `a`/`b` and the line is data.
        let results = parser.parse(b"c,d\n", 100_000);
        assert_eq!(results.len(), 1, "established header must not be replaced");
        assert_eq!(
            results[0].channels.get("a"),
            Some(&ChannelValue::State("c".to_string()))
        );
        assert_eq!(
            results[0].channels.get("b"),
            Some(&ChannelValue::State("d".to_string()))
        );

        // And the labels still apply to numeric data afterwards.
        let results = parser.parse(b"3,4\n", 100_100);
        assert_eq!(
            results[0].channels.get("a"),
            Some(&ChannelValue::Numeric(3.0))
        );
        assert_eq!(
            results[0].channels.get("b"),
            Some(&ChannelValue::Numeric(4.0))
        );
    }

    // ================================================================
    // Byte-scan boundaries (parse): line cap, discard, CR/LF pairing
    // ================================================================

    /// Mirrors `MAX_LINE_BUFFER` in `parse`.
    const MAX_LINE_BUFFER: usize = 64 * 1024;

    /// Fixed timestamp for the byte-scan tests: all calls share it, so the
    /// 2 s device-reset re-arm never interferes with what is being measured.
    const SCAN_TS: u64 = 1000;

    /// Build a line body of exactly `len` bytes that parses to `ch0 = value`.
    /// (`42` padded with spaces; runs of spaces collapse to one separator.)
    fn padded_line(value: &str, len: usize) -> Vec<u8> {
        let mut v = value.as_bytes().to_vec();
        v.resize(len, b' ');
        v
    }

    /// BVA: a line of EXACTLY 64 KiB is at the cap, not over it - it stays
    /// buffered and parses once its line ending arrives.
    #[test]
    fn test_line_of_exactly_max_buffer_is_kept() {
        let mut parser = PlotterParser::new();

        let line = padded_line("42", MAX_LINE_BUFFER);
        assert_eq!(line.len(), MAX_LINE_BUFFER);
        let results = parser.parse(&line, SCAN_TS);
        assert_eq!(results.len(), 0, "incomplete line yields nothing yet");

        let results = parser.parse(b"\n", SCAN_TS);
        assert_eq!(results.len(), 1, "a 64 KiB line must not be dropped");
        assert_eq!(results[0].channels.len(), 1);
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::Numeric(42.0))
        );
    }

    /// BVA: one byte over the cap is dropped, and everything up to the next
    /// line ending is discarded with it - then the stream recovers exactly.
    #[test]
    fn test_line_over_max_buffer_is_dropped_and_stream_recovers() {
        let mut parser = PlotterParser::new();

        let line = padded_line("43", MAX_LINE_BUFFER + 1);
        assert_eq!(line.len(), MAX_LINE_BUFFER + 1);
        let results = parser.parse(&line, SCAN_TS);
        assert_eq!(results.len(), 0);

        // "TAIL" is the rest of the oversized line: it must NOT become a
        // data point of its own. "7,8" is the first real line after it.
        let results = parser.parse(b"TAIL\n7,8\n", SCAN_TS);
        assert_eq!(
            results.len(),
            1,
            "only the line after the dropped one may be parsed"
        );
        assert_eq!(results[0].channels.len(), 2);
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::Numeric(7.0))
        );
        assert_eq!(
            results[0].channels.get("ch1"),
            Some(&ChannelValue::Numeric(8.0))
        );
    }

    /// The discard-until-newline drain is positioned ON the newline, so a
    /// recovery chunk whose FIRST byte is the newline loses nothing.
    #[test]
    fn test_discard_recovery_when_newline_is_first_byte() {
        let mut parser = PlotterParser::new();

        // Numeric line first, so the later "A,B" cannot be taken as a header.
        let results = parser.parse(b"1\n", SCAN_TS);
        assert_eq!(results.len(), 1);

        let results = parser.parse(&vec![b'7'; MAX_LINE_BUFFER + 1], SCAN_TS);
        assert_eq!(results.len(), 0, "oversized line is dropped");

        let results = parser.parse(b"\nA,B\n", SCAN_TS);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].channels.len(), 2);
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::State("A".to_string()))
        );
        assert_eq!(
            results[0].channels.get("ch1"),
            Some(&ChannelValue::State("B".to_string()))
        );
        assert_eq!(results[0].channel_order, vec!["ch0", "ch1"]);
    }

    /// A CRLF split across two reads (`\r` last byte of one chunk, `\n` first
    /// byte of the next) must produce exactly one point, and the line after
    /// it must parse normally.
    #[test]
    fn test_crlf_split_across_chunks() {
        let mut parser = PlotterParser::new();

        let results = parser.parse(b"1,2\r", SCAN_TS);
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::Numeric(1.0))
        );
        assert_eq!(
            results[0].channels.get("ch1"),
            Some(&ChannelValue::Numeric(2.0))
        );

        let results = parser.parse(b"\n3,4\n", SCAN_TS);
        assert_eq!(results.len(), 1, "the stray \\n must not add a point");
        assert_eq!(results[0].channels.len(), 2);
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::Numeric(3.0))
        );
        assert_eq!(
            results[0].channels.get("ch1"),
            Some(&ChannelValue::Numeric(4.0))
        );
    }

    /// A `\r` that is the very last byte of the whole input terminates its
    /// line; the leftover `\n` in the next chunk creates no phantom point and
    /// does not swallow the line that follows it.
    #[test]
    fn test_cr_at_end_of_input_then_lf_keeps_next_line() {
        let mut parser = PlotterParser::new();

        let results = parser.parse(b"5\r", SCAN_TS);
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::Numeric(5.0))
        );

        let results = parser.parse(b"\nx\n", SCAN_TS);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].channels.len(), 1);
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::State("x".to_string()))
        );
    }

    /// `\r` followed by a byte other than `\n` ends the line on its own: the
    /// following byte starts a new line.
    #[test]
    fn test_cr_followed_by_non_lf_starts_new_line() {
        let mut parser = PlotterParser::new();
        let results = parser.parse(b"1\r2\n", SCAN_TS);

        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::Numeric(1.0))
        );
        assert_eq!(
            results[1].channels.get("ch0"),
            Some(&ChannelValue::Numeric(2.0))
        );
    }

    /// `\n` followed by `\r` is two separate line endings, not a pair: the
    /// byte after the `\r` must not be swallowed.
    #[test]
    fn test_lf_then_cr_does_not_swallow_next_line() {
        let mut parser = PlotterParser::new();
        let results = parser.parse(b"1\n\r2\n", SCAN_TS);

        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::Numeric(1.0))
        );
        assert_eq!(
            results[1].channels.get("ch0"),
            Some(&ChannelValue::Numeric(2.0))
        );
    }

    /// A chunk that STARTS with a CRLF pair (index 0): the pair look-ahead
    /// must inspect the byte after the `\r`, never before it.
    #[test]
    fn test_chunk_starting_with_crlf() {
        let mut parser = PlotterParser::new();
        let results = parser.parse(b"\r\n1,2\n", SCAN_TS);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].channels.len(), 2);
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::Numeric(1.0))
        );
        assert_eq!(
            results[0].channels.get("ch1"),
            Some(&ChannelValue::Numeric(2.0))
        );
    }

    /// Runs of line endings produce no empty data points.
    #[test]
    fn test_consecutive_newlines_produce_no_empty_points() {
        let mut parser = PlotterParser::new();
        let results = parser.parse(b"1\n\n\n2\n", SCAN_TS);

        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].channels.get("ch0"),
            Some(&ChannelValue::Numeric(1.0))
        );
        assert_eq!(
            results[1].channels.get("ch0"),
            Some(&ChannelValue::Numeric(2.0))
        );
    }

    // ================================================================
    // Property-based tests (proptest)
    // ================================================================
    //
    // The headline property is P1 (`prop_chunking_invariance`): `parse` is a
    // streaming decoder, so the way a byte stream is split into read chunks
    // must not be observable in its output. The multi-byte-UTF-8 corruption bug
    // that motivated the raw-byte `line_buffer` was exactly a violation of it.

    use proptest::prelude::*;

    /// Timestamp used for every `parse` call in the chunking properties.
    ///
    /// One fixed value (below the 2000 ms threshold measured from the initial
    /// `last_line_ts == 0`) keeps the device-reset header re-arm out of the
    /// picture, so the only thing that differs between the chunked run and the
    /// whole-buffer run is where the byte boundaries fall - which is precisely
    /// what these properties are about.
    const PROP_TS: u64 = 1000;

    /// Label pool for generated `name:value` rows. Includes multi-byte
    /// (Japanese) names so that chunk boundaries can land mid-character.
    const LABEL_POOL: &[&str] = &["temp", "humidity", "rpm", "温度", "湿度", "モータ"];

    /// Name/value joiners and pair separators the parser accepts
    const JOINERS: &[&str] = &[":", "=", " = ", ": ", " :"];
    const SEPARATORS: &[&str] = &[",", " ", "\t", ", ", "  "];

    /// State words emitted as bare lines or as labeled values (ASCII + Japanese).
    const STATE_POOL: &[&str] = &["RUNNING", "STOPPED", "OK", "ERROR", "運転中", "停止"];

    /// A numeric token: a finite f64 rendered with 0-3 decimals.
    fn numeric_token() -> impl Strategy<Value = String> {
        (-1.0e6f64..1.0e6f64, 0usize..=3).prop_map(|(v, p)| format!("{:.*}", p, v))
    }

    /// A value token for labeled rows: mostly numeric, sometimes a state word.
    fn value_token() -> impl Strategy<Value = String> {
        prop_oneof![
            3 => numeric_token(),
            1 => prop::sample::select(STATE_POOL).prop_map(|s| s.to_string()),
        ]
    }

    /// One generated line, WITHOUT its line ending.
    ///
    /// Every alternative yields valid UTF-8 and contains no `\r`/`\n`
    /// (line endings are attached by `document()`). Note that a `\r` or `\n`
    /// BYTE can only come from an actual `\r`/`\n` char in UTF-8, so generated
    /// line bodies can never accidentally introduce a line break.
    fn line_body() -> impl Strategy<Value = String> {
        prop_oneof![
            // CSV numeric row (1-5 columns)
            4 => prop::collection::vec(numeric_token(), 1..=5).prop_map(|v| v.join(",")),
            // Labeled row: `name:value` pairs
            3 => prop::collection::vec((prop::sample::select(LABEL_POOL), value_token()), 1..=4)
                .prop_map(|pairs| pairs
                    .into_iter()
                    .map(|(l, v)| format!("{}:{}", l, v))
                    .collect::<Vec<_>>()
                    .join(",")),
            // Labeled row with any joiner and separator (`V=30.58 I=0.8`, `a = 1\tb: 2`)
            3 => (
                prop::collection::vec(
                    (prop::sample::select(LABEL_POOL), prop::sample::select(JOINERS), value_token()),
                    1..=4,
                ),
                prop::sample::select(SEPARATORS),
            )
                .prop_map(|(pairs, sep)| pairs
                    .into_iter()
                    .map(|(l, j, v)| format!("{}{}{}", l, j, v))
                    .collect::<Vec<_>>()
                    .join(sep)),
            // Bare state word (banner / status line)
            2 => prop::sample::select(STATE_POOL).prop_map(|s| s.to_string()),
            // Junk text: any short valid-UTF-8 run with no line endings
            2 => prop::string::string_regex("[^\r\n]{0,24}").unwrap(),
        ]
    }

    fn line_ending() -> impl Strategy<Value = &'static str> {
        prop_oneof![Just("\r\n"), Just("\n"), Just("\r")]
    }

    /// A whole document: up to 12 terminated lines (each with its own randomly
    /// chosen ending) plus an optional unterminated trailing line.
    ///
    /// GENERATOR CONSTRAINT: documents are kept far below the parser's 64 KiB
    /// line cap. That cap is inherently chunking-sensitive by design (a line
    /// longer than the cap is dropped as soon as the buffer overflows, and the
    /// point at which that happens depends on how the reads were split), so
    /// over-cap lines are outside the domain of the invariance property.
    fn document() -> impl Strategy<Value = Vec<u8>> {
        (
            prop::collection::vec((line_body(), line_ending()), 0..12),
            prop::option::of(line_body()),
        )
            .prop_map(|(lines, tail)| {
                let mut s = String::new();
                for (body, ending) in lines {
                    s.push_str(&body);
                    s.push_str(ending);
                }
                if let Some(tail) = tail {
                    s.push_str(&tail);
                }
                s.into_bytes()
            })
    }

    /// Build an arbitrary partition of `bytes` into at most 8 chunks.
    ///
    /// Cut points are taken modulo `len + 1`, so a boundary may land inside a
    /// multi-byte UTF-8 sequence, between the `\r` and the `\n` of a CRLF pair,
    /// or at the very start/end (producing empty chunks).
    fn split_chunks(bytes: &[u8], raw_cuts: &[u16]) -> Vec<(usize, usize)> {
        let n = bytes.len();
        let mut cuts: Vec<usize> = raw_cuts.iter().map(|c| (*c as usize) % (n + 1)).collect();
        cuts.push(0);
        cuts.push(n);
        cuts.sort_unstable();
        cuts.dedup();
        let ranges: Vec<(usize, usize)> = cuts.windows(2).map(|w| (w[0], w[1])).collect();
        if ranges.is_empty() {
            // Empty document: still exercise a single empty parse call.
            vec![(0, 0)]
        } else {
            ranges
        }
    }

    /// Order-stable, comparable projection of a parse result.
    type PointShape = (u64, Vec<String>, Vec<(String, ChannelValue)>);

    fn normalize(points: &[ParsedDataPoint]) -> Vec<PointShape> {
        points
            .iter()
            .map(|p| {
                let mut channels: Vec<(String, ChannelValue)> = p
                    .channels
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                channels.sort_by(|a, b| a.0.cmp(&b.0));
                (p.timestamp_ms, p.channel_order.clone(), channels)
            })
            .collect()
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

        /// P4 - named pairs in any joiner/separator style come back exactly:
        /// every pair becomes its name (in order) with its value, numbers with a
        /// unit suffix keep their number, and no other channel appears.
        #[test]
        fn prop_named_pairs_roundtrip(
            pairs in prop::collection::vec(
                (
                    prop::sample::select(LABEL_POOL),
                    prop::sample::select(JOINERS),
                    -1.0e6f64..1.0e6f64,
                    0usize..=3,
                    prop::sample::select(&["", "ms", "V", "%", "dB", "]"][..]),
                ),
                1..=5,
            ),
            sep in prop::sample::select(SEPARATORS),
        ) {
            let fields: Vec<String> = pairs
                .iter()
                .map(|(l, j, v, p, unit)| format!("{}{}{:.*}{}", l, j, p, v, unit))
                .collect();
            let line = format!("{}\n", fields.join(sep));

            let mut parser = PlotterParser::new();
            let points = parser.parse(line.as_bytes(), PROP_TS);

            prop_assert_eq!(points.len(), 1, "line {:?}", line);
            let want_names: Vec<String> = pairs.iter().map(|(l, ..)| l.to_string()).collect();
            prop_assert_eq!(&points[0].channel_order, &want_names, "line {:?}", line);
            // With duplicate names the last value wins (HashMap insert)
            if let Some((l, _, v, p, _)) = pairs.last() {
                let want = format!("{:.*}", p, v).parse::<f64>().unwrap();
                if let Some(ChannelValue::Numeric(got)) = points[0].channels.get(*l) {
                    prop_assert_eq!(*got, want, "{} in {:?}", l, line);
                } else {
                    prop_assert!(false, "{} not numeric in {:?}", l, line);
                }
            }
        }

        /// P1 - THE key property: chunking invariance.
        ///
        /// Guards: split-read handling. Feeding a byte stream through one
        /// parser in an arbitrary number of arbitrarily-placed chunks must
        /// produce exactly the same data points as feeding the whole buffer to
        /// a fresh parser in one call. Boundaries deliberately fall inside
        /// multi-byte UTF-8 characters and inside CRLF pairs.
        #[test]
        fn prop_chunking_invariance(
            bytes in document(),
            raw_cuts in prop::collection::vec(any::<u16>(), 0..8),
        ) {
            let mut chunked_parser = PlotterParser::new();
            let mut chunked: Vec<ParsedDataPoint> = Vec::new();
            for (a, b) in split_chunks(&bytes, &raw_cuts) {
                chunked.extend(chunked_parser.parse(&bytes[a..b], PROP_TS));
            }

            let mut whole_parser = PlotterParser::new();
            let whole = whole_parser.parse(&bytes, PROP_TS);

            prop_assert_eq!(
                normalize(&chunked),
                normalize(&whole),
                "chunked parse diverged from whole parse for input {:?}",
                String::from_utf8_lossy(&bytes)
            );
        }

        /// P2 - robustness on arbitrary (possibly non-UTF-8) bytes.
        ///
        /// Guards: no panic on any byte sequence, and the structural invariant
        /// that `channel_order` is non-empty and every label it lists is also
        /// present in `channels`.
        ///
        /// This property deliberately does NOT compare chunked vs whole output:
        /// for invalid UTF-8, `String::from_utf8_lossy` groups U+FFFD
        /// replacements per complete line, and a truncated sequence that spans
        /// a dropped/oversized boundary can legitimately produce a different
        /// number of replacement characters. That only ever affects `State`
        /// text, never numeric parsing.
        #[test]
        fn prop_parser_never_panics_on_arbitrary_bytes(
            bytes in prop::collection::vec(any::<u8>(), 0..3000),
            raw_cuts in prop::collection::vec(any::<u16>(), 0..8),
        ) {
            let mut parser = PlotterParser::new();
            for (a, b) in split_chunks(&bytes, &raw_cuts) {
                for point in parser.parse(&bytes[a..b], PROP_TS) {
                    prop_assert!(
                        !point.channel_order.is_empty(),
                        "data point emitted with an empty channel_order"
                    );
                    for label in &point.channel_order {
                        prop_assert!(
                            point.channels.contains_key(label),
                            "channel_order lists {:?} but channels has no such key",
                            label
                        );
                    }
                }
            }
        }

        /// P3 - numeric round-trip.
        ///
        /// Guards: for a pure CSV numeric document every column parses as
        /// `Numeric` and is bit-for-bit the value obtained by re-parsing the
        /// same rendered token, i.e. no precision is lost or invented between
        /// the wire format and `ChannelValue::Numeric`.
        #[test]
        fn prop_numeric_roundtrip(
            rows in prop::collection::vec(
                prop::collection::vec((-1.0e6f64..1.0e6f64, 0usize..=3), 1..=5),
                1..12,
            ),
        ) {
            let mut text = String::new();
            let mut expected: Vec<Vec<f64>> = Vec::with_capacity(rows.len());
            for row in &rows {
                let rendered: Vec<String> =
                    row.iter().map(|(v, p)| format!("{:.*}", p, v)).collect();
                expected.push(
                    rendered
                        .iter()
                        .map(|s| s.parse::<f64>().expect("rendered token must re-parse"))
                        .collect(),
                );
                text.push_str(&rendered.join(","));
                text.push('\n');
            }

            let mut parser = PlotterParser::new();
            let points = parser.parse(text.as_bytes(), PROP_TS);

            prop_assert_eq!(points.len(), expected.len(), "row count mismatch");

            for (point, want) in points.iter().zip(expected.iter()) {
                prop_assert_eq!(point.channel_order.len(), want.len(), "column count mismatch");
                for (i, w) in want.iter().enumerate() {
                    let label = format!("ch{}", i);
                    match point.channels.get(&label) {
                        Some(ChannelValue::Numeric(got)) => {
                            prop_assert_eq!(*got, *w, "column {} value mismatch", i);
                        }
                        other => {
                            prop_assert!(
                                false,
                                "column {} parsed as {:?}, expected Numeric({})",
                                i,
                                other,
                                w
                            );
                        }
                    }
                }
            }
        }
    }

    // ---- untrusted segment starts (docs/27 PF-2) ----

    fn names(points: &[ParsedDataPoint]) -> Vec<String> {
        let mut all: Vec<String> = points
            .iter()
            .flat_map(|p| p.channels.keys().cloned())
            .collect();
        all.sort();
        all.dedup();
        all
    }

    /// The observed case: a cut line left in the buffer joins the first line
    /// after the boundary. It must neither create a phantom channel nor turn
    /// `cos` into a state channel
    #[test]
    fn test_segment_start_drops_cut_line_reproduction() {
        let mut parser = PlotterParser::new();
        assert!(parser.parse(b"sin:39.05,cos:", 0).is_empty());
        parser.begin_untrusted_segment();
        let points = parser.parse(
            b"accel:0.049,temp:24,state:IDLE\r\nsin:1,cos:2,accel:3,temp:4,state:IDLE\r\n",
            10,
        );
        assert_eq!(points.len(), 1);
        assert_eq!(names(&points), vec!["accel", "cos", "sin", "state", "temp"]);
        assert_eq!(
            points[0].channels.get("cos"),
            Some(&ChannelValue::Numeric(2.0))
        );
    }

    /// Connecting mid-line to a CSV stream: the cut first line is dropped
    #[test]
    fn test_segment_start_drops_cut_csv_line() {
        let mut parser = PlotterParser::new();
        parser.begin_untrusted_segment();
        let points = parser.parse(b"39.05,cos:0.5\n1,2\n", 0);
        assert_eq!(points.len(), 1);
        assert_eq!(names(&points), vec!["ch0", "ch1"]);
    }

    /// CR-only line endings release the segment start too (a parser that only
    /// looked for LF would drop everything forever)
    #[test]
    fn test_segment_start_with_cr_only_endings() {
        let mut parser = PlotterParser::new();
        parser.begin_untrusted_segment();
        assert_eq!(parser.parse(b"cut\r1\r2\r", 0).len(), 2);
    }

    /// A CRLF split across reads does not leave an empty phantom line
    #[test]
    fn test_segment_start_crlf_split_across_reads() {
        let mut parser = PlotterParser::new();
        parser.begin_untrusted_segment();
        assert!(parser.parse(b"cut\r", 0).is_empty());
        let points = parser.parse(b"\n5\n", 10);
        assert_eq!(points.len(), 1);
        assert_eq!(
            points[0].channels.get("ch0"),
            Some(&ChannelValue::Numeric(5.0))
        );
    }

    /// An empty first line releases it: the next line is data
    #[test]
    fn test_segment_start_released_by_empty_line() {
        let mut parser = PlotterParser::new();
        parser.begin_untrusted_segment();
        assert_eq!(parser.parse(b"\n7\n", 0).len(), 1);
    }

    /// A clean CSV header at the segment start is still adopted (a board reset
    /// by DTR prints its header first), and a boot banner does not block it
    #[test]
    fn test_segment_start_keeps_a_csv_header() {
        let mut parser = PlotterParser::new();
        parser.begin_untrusted_segment();
        let points = parser.parse(b"a,b\n1,2\n", 0);
        assert_eq!(names(&points), vec!["a", "b"]);

        let mut parser = PlotterParser::new();
        parser.begin_untrusted_segment();
        // (a two-word banner such as "Boot OK" is adopted as a header even on a
        // clean start - an existing quirk, unchanged here)
        let points = parser.parse(b"READY\na,b\n1,2\n", 0);
        assert_eq!(names(&points), vec!["a", "b"]);
    }

    /// A cut labelled line is never mistaken for a header
    #[test]
    fn test_segment_start_cut_labelled_line_is_not_a_header() {
        let mut parser = PlotterParser::new();
        parser.begin_untrusted_segment();
        let points = parser.parse(b"cos:accel:0.049,temp:24\n1,2\n", 0);
        assert_eq!(names(&points), vec!["ch0", "ch1"]);
    }

    // ---- Flexible name/value recognition (owner request after issue #3's data) ----

    fn parse_one(line: &str) -> Option<ParsedDataPoint> {
        let mut parser = PlotterParser::new();
        let mut points = parser.parse(format!("{}\n", line).as_bytes(), 0);
        assert!(points.len() <= 1);
        points.pop()
    }

    fn num(p: &ParsedDataPoint, name: &str) -> Option<f64> {
        match p.channels.get(name) {
            Some(ChannelValue::Numeric(v)) => Some(*v),
            _ => None,
        }
    }

    /// The tester's firmware line: `name=value` pairs separated by spaces
    #[test]
    fn test_equals_pairs_from_real_firmware() {
        let line = "[Mon t=2916094] V=30.58 Vdc=12.37 I=0.825/0.797 ph=49.5/1 M=0.86996 \
                    phi=-1.35 pll=49.9996 pe=-3.2 r=1/1 age=11ms rdy=1 cap=4084 md=687 sd=23";
        let p = parse_one(line).unwrap();
        assert_eq!(
            p.channel_order,
            vec!["t", "V", "Vdc", "M", "phi", "pll", "pe", "age", "rdy", "cap", "md", "sd"]
        );
        assert_eq!(num(&p, "t"), Some(2916094.0));
        assert_eq!(num(&p, "V"), Some(30.58));
        assert_eq!(num(&p, "phi"), Some(-1.35));
        assert_eq!(num(&p, "age"), Some(11.0));
        // "0.825/0.797" etc. are neither numbers nor states; "[Mon" is decoration
        for gone in ["I", "ph", "r", "ch0", "[Mon"] {
            assert!(!p.channels.contains_key(gone), "{gone}");
        }
    }

    /// ...and as the first line it is data, not a header (also when untrusted)
    #[test]
    fn test_equals_line_is_never_a_header() {
        let mut parser = PlotterParser::new();
        let points = parser.parse(b"a=x b=y\n", 0);
        assert_eq!(names(&points), vec!["a", "b"]);

        let mut parser = PlotterParser::new();
        parser.begin_untrusted_segment();
        let points = parser.parse(b"V=1 I=2\n1,2\n", 0);
        assert_eq!(names(&points), vec!["ch0", "ch1"]);
    }

    #[test]
    fn test_joiners_with_spaces() {
        for line in [
            "temp = 25 hum = 60",
            "temp: 25 hum: 60",
            "temp =25 hum= 60",
            "temp = 25, hum = 60",
            "temp=25\thum:60",
            "temp 25 hum 60",
        ] {
            let p = parse_one(line).unwrap();
            assert_eq!(p.channel_order, vec!["temp", "hum"], "{line}");
            assert_eq!(num(&p, "temp"), Some(25.0), "{line}");
            assert_eq!(num(&p, "hum"), Some(60.0), "{line}");
        }
    }

    #[test]
    fn test_units_after_pairs_are_dropped() {
        let p = parse_one("Temp: 25.3 C Hum: 60 %").unwrap();
        assert_eq!(p.channel_order, vec!["Temp", "Hum"]);
        assert_eq!(num(&p, "Temp"), Some(25.3));
    }

    #[test]
    fn test_time_of_day_is_not_a_pair() {
        for line in ["12:34:56 temp=25", "[12:34:56] temp=25"] {
            let p = parse_one(line).unwrap();
            assert_eq!(p.channel_order, vec!["temp"], "{line}");
        }
        assert!(parse_one("t=12:34:56").is_none());
    }

    #[test]
    fn test_value_classification() {
        let n = |s: &str| classify_value(s);
        for (s, v) in [
            ("11ms", 11.0),
            ("3.3V", 3.3),
            ("100%", 100.0),
            ("-3.2dB", -3.2),
            ("2916094]", 2916094.0),
            ("1e3", 1000.0),
        ] {
            assert_eq!(n(s), Some(ChannelValue::Numeric(v)), "{s}");
        }
        for s in ["RUN", "MODE2", "E01", "NaN", "運転中"] {
            assert_eq!(n(s), Some(ChannelValue::State(s.to_string())), "{s}");
        }
        for s in [
            "0.825/0.797",
            "1/1",
            "0x1F",
            "-inf",
            "12:34:56",
            "",
            "[Mon",
            "accel:0.049",
        ] {
            assert_eq!(n(s), None, "{s}");
        }
    }

    /// `key=` with no value disappears; it does not become a positional State
    #[test]
    fn test_empty_values_are_dropped() {
        // (a value-less "k=" directly before another pair is closed up into it:
        // "k= x=1" reads as k = "x=1", which is dropped as well)
        for line in ["x=1 k=", "x=1 k:", "k=,x=1"] {
            let p = parse_one(line).unwrap();
            assert_eq!(p.channel_order, vec!["x"], "{line}");
        }
        assert!(parse_one("k=").is_none());
    }

    /// Space joiner only when every bare number follows a word
    #[test]
    fn test_space_joiner_guards() {
        let p = parse_one("25.5 60 RUNNING").unwrap();
        assert_eq!(p.channel_order, vec!["ch0", "ch1", "ch2"]);
        assert_eq!(
            p.channels.get("ch2"),
            Some(&ChannelValue::State("RUNNING".into()))
        );

        let p = parse_one("RUNNING 25.5 60").unwrap();
        assert_eq!(p.channel_order, vec!["ch0", "ch1", "ch2"]);

        // With a header the columns are named by it
        let mut parser = PlotterParser::new();
        let points = parser.parse(b"state temp hum\nON 25.5 60\n", 0);
        assert_eq!(points[0].channel_order, vec!["state", "temp", "hum"]);
        assert_eq!(
            points[0].channels.get("state"),
            Some(&ChannelValue::State("ON".into()))
        );

        // Prose: the number is named by the word before it, the rest is dropped
        let p = parse_one("Error 5 occurred").unwrap();
        assert_eq!(p.channel_order, vec!["Error"]);
        assert_eq!(num(&p, "Error"), Some(5.0));
    }

    /// In comma/tab lines a bare word stays a positional state column
    #[test]
    fn test_bare_state_column_kept_in_csv() {
        let p = parse_one("temp:25.5,RUNNING").unwrap();
        assert_eq!(p.channel_order, vec!["temp", "ch1"]);
        assert_eq!(
            p.channels.get("ch1"),
            Some(&ChannelValue::State("RUNNING".into()))
        );
    }

    #[test]
    fn test_header_rules() {
        let mut parser = PlotterParser::new();
        assert!(parser.parse(b"a,b,c\n", 0).is_empty()); // header
        let mut parser = PlotterParser::new();
        assert!(parser.parse(b"temp hum\n", 0).is_empty()); // header
        let mut parser = PlotterParser::new();
        assert_eq!(names(&parser.parse(b"a=1,b=2\n", 0)), vec!["a", "b"]);
    }

    /// Names start with a letter (any script) or '_'
    #[test]
    fn test_name_rule() {
        assert_eq!(parse_one("温度=25").unwrap().channel_order, vec!["温度"]);
        assert_eq!(parse_one("_x=1").unwrap().channel_order, vec!["_x"]);
        // A digit-first label is not a name: the field is not a pair
        assert!(parse_one("3V3=3.3").is_none());
    }
}
