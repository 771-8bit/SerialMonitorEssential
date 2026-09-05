// Plotter Parser - Parse serial data into plottable values
//
// Supports:
// - Single values: `123.45\r\n`
// - CSV format: `10,20,30\r\n`
// - Tab/space separated: `10\t20\t30\r\n`
// - Labeled values: `temp:25.5,humidity:60\r\n`
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
        }
    }

    /// Reset parser state (for new session)
    pub fn reset(&mut self) {
        self.labels.clear();
        self.header_detected = false;
        self.saw_numeric_data = false;
        self.line_buffer.clear();
        self.discard_until_newline = false;
        self.last_line_ts = 0;
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
                if i > line_start {
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
        }

        results
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

        // For space separation, collapse runs of spaces (printf-style aligned
        // output) instead of producing empty parts that shift column indices.
        let parts: Vec<&str> = if separator == ' ' {
            line.split_whitespace().collect()
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

        // Parse values
        let mut channels = HashMap::new();
        let mut channel_order = Vec::new();

        for (i, part) in parts.iter().enumerate() {
            // Check for labeled value (label:value format)
            if let Some((label, value)) = self.parse_labeled_value(part) {
                channels.insert(label.clone(), value);
                channel_order.push(label);
            } else {
                // Auto-generate label
                let label = if i < self.labels.len() {
                    self.labels[i].clone()
                } else {
                    format!("ch{}", i)
                };

                if let Some(value) = self.parse_value(part) {
                    channels.insert(label.clone(), value);
                    channel_order.push(label);
                }
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

        // If any part contains ':', it's a labeled value line, not a header
        if parts.iter().any(|part| part.contains(':')) {
            return false;
        }

        // All parts must be non-numeric and non-empty
        parts.iter().all(|part| {
            let trimmed = part.trim();
            !trimmed.is_empty() && trimmed.parse::<f64>().is_err()
        })
    }

    /// Parse a labeled value (label:value format)
    fn parse_labeled_value(&self, s: &str) -> Option<(String, ChannelValue)> {
        let parts: Vec<&str> = s.splitn(2, ':').collect();
        if parts.len() != 2 {
            return None;
        }

        let label = parts[0].trim().to_string();
        let value_str = parts[1].trim();

        if label.is_empty() {
            return None;
        }

        let value = self.parse_value(value_str)?;
        Some((label, value))
    }

    /// Parse a value as numeric or state
    fn parse_value(&self, s: &str) -> Option<ChannelValue> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return None;
        }

        // Try parsing as number
        if let Ok(num) = trimmed.parse::<f64>() {
            // Only accept finite numbers as Numeric
            if num.is_finite() {
                return Some(ChannelValue::Numeric(num));
            }
            // Non-finite (NaN, Infinity) fall through to State
        }

        // Treat as state value
        Some(ChannelValue::State(trimmed.to_string()))
    }
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
}
