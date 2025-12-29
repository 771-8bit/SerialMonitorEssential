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
    /// Detected separator (auto-detected from first line with data)
    separator: Option<char>,
    /// Channel labels (from header or auto-generated)
    labels: Vec<String>,
    /// Whether header has been detected
    header_detected: bool,
    /// Buffer for incomplete lines
    line_buffer: String,
}

impl Default for PlotterParser {
    fn default() -> Self {
        Self::new()
    }
}

impl PlotterParser {
    pub fn new() -> Self {
        Self {
            separator: None,
            labels: Vec::new(),
            header_detected: false,
            line_buffer: String::new(),
        }
    }

    /// Reset parser state (for new session)
    pub fn reset(&mut self) {
        self.separator = None;
        self.labels.clear();
        self.header_detected = false;
        self.line_buffer.clear();
    }

    /// Parse incoming bytes into data points
    ///
    /// Returns a vector of parsed data points. Each complete line produces one data point.
    /// Incomplete lines are buffered for the next call.
    pub fn parse(&mut self, data: &[u8], timestamp_ms: u64) -> Vec<ParsedDataPoint> {
        let mut results = Vec::new();

        // Convert bytes to string (lossy for non-UTF8)
        let text = String::from_utf8_lossy(data);
        self.line_buffer.push_str(&text);

        // Process complete lines
        while let Some(line_end) = self.find_line_end(&self.line_buffer.clone()) {
            let line = self.line_buffer[..line_end].to_string();
            let skip_len = self.skip_newline_chars(&self.line_buffer[line_end..]);
            self.line_buffer = self.line_buffer[line_end + skip_len..].to_string();

            if line.trim().is_empty() {
                continue;
            }

            if let Some(data_point) = self.parse_line(&line, timestamp_ms) {
                results.push(data_point);
            }
        }

        results
    }

    /// Find the position of line ending (CR, LF, or CRLF)
    fn find_line_end(&self, s: &str) -> Option<usize> {
        for (i, c) in s.char_indices() {
            if c == '\r' || c == '\n' {
                return Some(i);
            }
        }
        None
    }

    /// Count how many newline characters to skip
    fn skip_newline_chars(&self, s: &str) -> usize {
        let mut count = 0;
        for c in s.chars() {
            if c == '\r' || c == '\n' {
                count += c.len_utf8();
                // Handle CRLF as single newline
                if c == '\r' && s.chars().nth(1) == Some('\n') {
                    count += 1;
                    break;
                }
            } else {
                break;
            }
        }
        count.max(1) // At least skip 1 character
    }

    /// Parse a single line into a data point
    fn parse_line(&mut self, line: &str, timestamp_ms: u64) -> Option<ParsedDataPoint> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }

        // Detect separator if not yet detected
        if self.separator.is_none() {
            self.separator = Some(self.detect_separator(line));
        }

        let separator = self.separator.unwrap();
        let parts: Vec<&str> = line.split(separator).map(|s| s.trim()).collect();

        // Check if this is a header line (all non-numeric)
        if !self.header_detected && self.is_header_line(&parts) {
            self.labels = parts.iter().map(|s| s.to_string()).collect();
            self.header_detected = true;
            return None; // Don't return data point for header
        }

        // Parse values
        let mut channels = HashMap::new();

        for (i, part) in parts.iter().enumerate() {
            // Check for labeled value (label:value format)
            if let Some((label, value)) = self.parse_labeled_value(part) {
                channels.insert(label, value);
            } else {
                // Auto-generate label
                let label = if i < self.labels.len() {
                    self.labels[i].clone()
                } else {
                    format!("ch{}", i)
                };

                if let Some(value) = self.parse_value(part) {
                    channels.insert(label, value);
                }
            }
        }

        if channels.is_empty() {
            return None;
        }

        Some(ParsedDataPoint {
            timestamp_ms,
            channels,
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
}
