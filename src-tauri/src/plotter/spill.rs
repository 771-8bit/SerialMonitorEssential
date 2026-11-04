//! Plotter disk spill (SYS-F-518 / ADR-17).
//!
//! The in-memory tiers (ADR-16) get coarser with age, which is fine for LIVE
//! windows but not for zooming into old data. Every 1 ms cell that leaves tier 0
//! is therefore appended to per-channel files, at three levels of detail
//! (1, 64 and 4096 ms cells), so a zoomed static range can always be drawn at
//! the chart's native resolution (1 ms) while reading at most ~64 records per
//! display cell.
//!
//! - Writing happens on a dedicated thread; the aggregator only hands over
//!   finished cells (no file I/O under the aggregator lock).
//! - Records are fixed-size and sorted by time; a sparse index (one entry per
//!   `INDEX_EVERY` records) locates the start of a range.
//! - A reader first calls `sync()` (the writer flushes everything it has been
//!   given), then reads only up to what was flushed, so it never sees a torn
//!   record nor misses a cell in flight.
//! - One `SpillLog` = one generation (one plotter session). Dropping it stops
//!   the writer and deletes its directory; readers hold an `Arc`, so the files
//!   outlive a Clear until the last reader is done.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam::channel::{self, Receiver, RecvTimeoutError, Sender};

/// Cell widths of the spill levels. Neighbouring levels differ by 64x, which
/// bounds the records read for one display cell (ADR-17).
pub(crate) const LEVEL_MS: [u64; 3] = [1, 64, 4096];

const RECORD_BYTES: usize = 48;
const INDEX_EVERY: u64 = 1024;
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);
const SYNC_TIMEOUT: Duration = Duration::from_secs(2);

/// Default cap for all spill files of one session (4 GiB)
pub(crate) const DEFAULT_MAX_BYTES: u64 = 4 << 30;

/// One stored cell: extremes with their times, sum and count
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SpillCell {
    /// Start of the cell on its level's grid
    pub ts: u64,
    pub min: f64,
    pub max: f64,
    pub sum: f64,
    pub count: u64,
    pub ts_min: u64,
    pub ts_max: u64,
}

impl SpillCell {
    /// Same tie rule as the in-memory merge: smaller (larger) value, then the
    /// earlier time, so levels agree with the tiers bit for bit.
    fn absorb(&mut self, other: &SpillCell) {
        if other.min < self.min || (other.min == self.min && other.ts_min < self.ts_min) {
            self.min = other.min;
            self.ts_min = other.ts_min;
        }
        if other.max > self.max || (other.max == self.max && other.ts_max < self.ts_max) {
            self.max = other.max;
            self.ts_max = other.ts_max;
        }
        self.sum += other.sum;
        self.count += other.count;
    }

    fn encode(&self) -> [u8; RECORD_BYTES] {
        let mut out = [0u8; RECORD_BYTES];
        out[0..8].copy_from_slice(&self.ts.to_le_bytes());
        out[8..16].copy_from_slice(&self.min.to_le_bytes());
        out[16..24].copy_from_slice(&self.max.to_le_bytes());
        out[24..32].copy_from_slice(&self.sum.to_le_bytes());
        out[32..40].copy_from_slice(&self.count.to_le_bytes());
        // Extreme times as offsets from the cell start (< 4096 ms fits easily)
        out[40..44].copy_from_slice(&((self.ts_min - self.ts) as u32).to_le_bytes());
        out[44..48].copy_from_slice(&((self.ts_max - self.ts) as u32).to_le_bytes());
        out
    }

    fn decode(bytes: &[u8]) -> Self {
        let u64_at = |i: usize| u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        let f64_at = |i: usize| f64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        let u32_at = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap()) as u64;
        let ts = u64_at(0);
        Self {
            ts,
            min: f64_at(8),
            max: f64_at(16),
            sum: f64_at(24),
            count: u64_at(32),
            ts_min: ts + u32_at(40),
            ts_max: ts + u32_at(44),
        }
    }
}

/// What readers may see of one level file
#[derive(Debug, Default)]
struct LevelMeta {
    /// Records fully written and flushed
    flushed_records: u64,
    /// (ts, record number) every INDEX_EVERY records
    index: Vec<(u64, u64)>,
    /// Every cell starting before this time is in the file (flushed)
    complete_until: u64,
}

/// Reader-visible state of one channel: per level metadata
#[derive(Debug, Default)]
struct ChannelMeta {
    levels: [Mutex<LevelMeta>; 3],
}

enum Msg {
    Cells {
        channel: usize,
        cells: Vec<SpillCell>,
    },
    Sync(Sender<()>),
}

/// Disk spill of one plotter session
pub(crate) struct SpillLog {
    dir: PathBuf,
    tx: Option<Sender<Msg>>,
    writer: Option<JoinHandle<()>>,
    channels: Arc<Mutex<Vec<Arc<ChannelMeta>>>>,
    /// Set once the byte cap was hit: nothing after this time was written.
    /// Readers do not need it (they stop at what was flushed); kept for tests.
    #[cfg_attr(not(test), allow(dead_code))]
    truncated: Arc<AtomicU64>,
    /// Number of range reads served (LIVE windows must never cause one)
    #[cfg_attr(not(test), allow(dead_code))]
    reads: AtomicU64,
}

impl SpillLog {
    /// Start a spill generation in a fresh directory under `base`
    pub(crate) fn create(base: &Path, max_bytes: u64) -> std::io::Result<Self> {
        static GENERATION: AtomicU64 = AtomicU64::new(0);
        let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
        let dir = base.join(format!("plotter-{generation}"));
        fs::create_dir_all(&dir)?;

        let (tx, rx) = channel::unbounded();
        let channels: Arc<Mutex<Vec<Arc<ChannelMeta>>>> = Arc::default();
        let truncated = Arc::new(AtomicU64::new(u64::MAX));
        let writer = {
            let dir = dir.clone();
            let channels = channels.clone();
            let truncated = truncated.clone();
            thread::Builder::new()
                .name("plotter-spill".into())
                .spawn(move || run_writer(rx, dir, channels, truncated, max_bytes))?
        };
        Ok(Self {
            dir,
            tx: Some(tx),
            writer: Some(writer),
            channels,
            truncated,
            reads: AtomicU64::new(0),
        })
    }

    /// Default location: next to the capture's temp files, per process
    pub(crate) fn create_default() -> std::io::Result<Self> {
        let base = std::env::temp_dir()
            .join("SerialMonitorEssential")
            .join(std::process::id().to_string());
        Self::create(&base, DEFAULT_MAX_BYTES)
    }

    /// Hand finished 1 ms cells of one channel to the writer (time order)
    pub(crate) fn append(&self, channel: usize, cells: Vec<SpillCell>) {
        if cells.is_empty() {
            return;
        }
        if let Some(tx) = &self.tx {
            let _ = tx.send(Msg::Cells { channel, cells });
        }
    }

    /// Wait until everything handed over so far is flushed and visible
    pub(crate) fn sync(&self) -> bool {
        let Some(tx) = &self.tx else { return false };
        let (done_tx, done_rx) = channel::bounded(1);
        if tx.send(Msg::Sync(done_tx)).is_err() {
            return false;
        }
        done_rx.recv_timeout(SYNC_TIMEOUT).is_ok()
    }

    /// Time from which nothing was written because the byte cap was hit
    /// (`u64::MAX` = never truncated)
    #[cfg(test)]
    pub(crate) fn truncated_from(&self) -> u64 {
        self.truncated.load(Ordering::Acquire)
    }

    /// Cells of `level` overlapping [t0, t1), up to what is complete on disk.
    /// Returns the cells and the time up to which the level is complete.
    pub(crate) fn read(
        &self,
        channel: usize,
        level: usize,
        t0: u64,
        t1: u64,
    ) -> (Vec<SpillCell>, u64) {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let meta = {
            let channels = self.channels.lock().unwrap_or_else(|e| e.into_inner());
            match channels.get(channel) {
                Some(meta) => meta.clone(),
                None => return (Vec::new(), 0),
            }
        };
        let (flushed, start_record, complete_until) = {
            let level_meta = meta.levels[level].lock().unwrap_or_else(|e| e.into_inner());
            // Start at the last indexed record that begins at or before t0 - width
            let from = t0.saturating_sub(LEVEL_MS[level]);
            let pos = level_meta.index.partition_point(|&(ts, _)| ts <= from);
            let start = if pos == 0 {
                0
            } else {
                level_meta.index[pos - 1].1
            };
            (level_meta.flushed_records, start, level_meta.complete_until)
        };
        let end = t1.min(complete_until);
        let mut out = Vec::new();
        if start_record >= flushed || t0 >= end {
            return (out, complete_until);
        }
        let path = level_path(&self.dir, channel, level);
        let Ok(mut file) = File::open(&path) else {
            return (out, complete_until);
        };
        if file
            .seek(SeekFrom::Start(start_record * RECORD_BYTES as u64))
            .is_err()
        {
            return (out, complete_until);
        }
        let mut remaining = flushed - start_record;
        let mut buf = vec![0u8; RECORD_BYTES * 4096];
        'outer: while remaining > 0 {
            let n = remaining.min(4096) as usize;
            if file.read_exact(&mut buf[..n * RECORD_BYTES]).is_err() {
                break;
            }
            remaining -= n as u64;
            for chunk in buf[..n * RECORD_BYTES].chunks_exact(RECORD_BYTES) {
                let cell = SpillCell::decode(chunk);
                if cell.ts >= end {
                    break 'outer;
                }
                if cell.ts + LEVEL_MS[level] > t0 {
                    out.push(cell);
                }
            }
        }
        (out, complete_until)
    }

    /// Directory of this generation (tests)
    #[cfg(test)]
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// Range reads served so far (tests)
    #[cfg(test)]
    pub(crate) fn reads(&self) -> u64 {
        self.reads.load(Ordering::Relaxed)
    }
}

impl Drop for SpillLog {
    fn drop(&mut self) {
        // Closing the channel ends the writer loop
        self.tx = None;
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
        if let Err(e) = fs::remove_dir_all(&self.dir) {
            log::warn!("[spill] Failed to remove {:?}: {}", self.dir, e);
        }
    }
}

impl std::fmt::Debug for SpillLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpillLog").field("dir", &self.dir).finish()
    }
}

fn level_path(dir: &Path, channel: usize, level: usize) -> PathBuf {
    dir.join(format!("ch{channel}-L{level}.bin"))
}

/// Writer-side state of one level file
struct LevelWriter {
    file: BufWriter<File>,
    written: u64,
    index: Vec<(u64, u64)>,
    /// Cell being accumulated (levels >= 1)
    acc: Option<SpillCell>,
    last_end: u64,
}

impl LevelWriter {
    fn write(&mut self, cell: &SpillCell, level: usize) -> std::io::Result<()> {
        if self.written.is_multiple_of(INDEX_EVERY) {
            self.index.push((cell.ts, self.written));
        }
        self.file.write_all(&cell.encode())?;
        self.written += 1;
        self.last_end = cell.ts + LEVEL_MS[level];
        Ok(())
    }

    fn complete_until(&self) -> u64 {
        self.acc.map_or(self.last_end, |acc| acc.ts)
    }
}

struct ChannelWriter {
    levels: Vec<LevelWriter>,
    meta: Arc<ChannelMeta>,
}

fn run_writer(
    rx: Receiver<Msg>,
    dir: PathBuf,
    channels: Arc<Mutex<Vec<Arc<ChannelMeta>>>>,
    truncated: Arc<AtomicU64>,
    max_bytes: u64,
) {
    let mut writers: Vec<Option<ChannelWriter>> = Vec::new();
    let mut bytes: u64 = 0;
    let mut last_flush = Instant::now();
    let mut dirty = false;

    loop {
        let msg = match rx.recv_timeout(FLUSH_INTERVAL) {
            Ok(msg) => Some(msg),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        match msg {
            Some(Msg::Cells { channel, cells }) => {
                if truncated.load(Ordering::Acquire) != u64::MAX {
                    continue;
                }
                if bytes >= max_bytes {
                    let from = cells.first().map_or(0, |c| c.ts);
                    log::warn!("[spill] Size cap reached; full resolution stops at {from} ms");
                    truncated.store(from, Ordering::Release);
                    continue;
                }
                let writer = match channel_writer(&mut writers, channel, &dir, &channels) {
                    Ok(writer) => writer,
                    Err(e) => {
                        log::warn!("[spill] Cannot open spill files: {e}");
                        continue;
                    }
                };
                for cell in &cells {
                    if let Err(e) = write_cell(writer, cell, &mut bytes) {
                        log::warn!("[spill] Write failed: {e}");
                        break;
                    }
                }
                dirty = true;
            }
            Some(Msg::Sync(done)) => {
                flush_all(&mut writers);
                dirty = false;
                last_flush = Instant::now();
                let _ = done.send(());
                continue;
            }
            None => {}
        }
        if dirty && last_flush.elapsed() >= FLUSH_INTERVAL {
            flush_all(&mut writers);
            dirty = false;
            last_flush = Instant::now();
        }
    }
    flush_all(&mut writers);
}

fn channel_writer<'a>(
    writers: &'a mut Vec<Option<ChannelWriter>>,
    channel: usize,
    dir: &Path,
    channels: &Mutex<Vec<Arc<ChannelMeta>>>,
) -> std::io::Result<&'a mut ChannelWriter> {
    if writers.len() <= channel {
        writers.resize_with(channel + 1, || None);
    }
    if writers[channel].is_none() {
        let mut levels = Vec::with_capacity(LEVEL_MS.len());
        for level in 0..LEVEL_MS.len() {
            levels.push(LevelWriter {
                file: BufWriter::with_capacity(
                    64 * 1024,
                    File::create(level_path(dir, channel, level))?,
                ),
                written: 0,
                index: Vec::new(),
                acc: None,
                last_end: 0,
            });
        }
        let meta = Arc::new(ChannelMeta::default());
        {
            let mut shared = channels.lock().unwrap_or_else(|e| e.into_inner());
            if shared.len() <= channel {
                shared.resize_with(channel + 1, || Arc::new(ChannelMeta::default()));
            }
            shared[channel] = meta.clone();
        }
        writers[channel] = Some(ChannelWriter { levels, meta });
    }
    Ok(writers[channel].as_mut().expect("just created"))
}

fn write_cell(
    writer: &mut ChannelWriter,
    cell: &SpillCell,
    bytes: &mut u64,
) -> std::io::Result<()> {
    writer.levels[0].write(cell, 0)?;
    *bytes += RECORD_BYTES as u64;
    for (level, (&width, level_writer)) in LEVEL_MS
        .iter()
        .zip(writer.levels.iter_mut())
        .enumerate()
        .skip(1)
    {
        let start = cell.ts / width * width;
        match level_writer.acc.as_mut() {
            Some(acc) if acc.ts == start => acc.absorb(cell),
            _ => {
                if let Some(done) = level_writer.acc.take() {
                    level_writer.write(&done, level)?;
                    *bytes += RECORD_BYTES as u64;
                }
                let mut fresh = *cell;
                fresh.ts = start;
                level_writer.acc = Some(fresh);
            }
        }
    }
    Ok(())
}

fn flush_all(writers: &mut [Option<ChannelWriter>]) {
    for writer in writers.iter_mut().flatten() {
        for (level, level_writer) in writer.levels.iter_mut().enumerate() {
            if let Err(e) = level_writer.file.flush() {
                log::warn!("[spill] Flush failed: {e}");
                continue;
            }
            let mut meta = writer.meta.levels[level]
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            meta.flushed_records = level_writer.written;
            meta.index.clone_from(&level_writer.index);
            meta.complete_until = level_writer.complete_until();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(ts: u64, v: f64) -> SpillCell {
        SpillCell {
            ts,
            min: v,
            max: v,
            sum: v,
            count: 1,
            ts_min: ts,
            ts_max: ts,
        }
    }

    fn test_log() -> SpillLog {
        let base = std::env::temp_dir()
            .join("SerialMonitorEssential_test")
            .join(format!("spill-{}", std::process::id()));
        SpillLog::create(&base, DEFAULT_MAX_BYTES).unwrap()
    }

    #[test]
    fn test_record_round_trip() {
        let c = SpillCell {
            ts: 4096,
            min: -1.5,
            max: 9.25,
            sum: 12.0,
            count: 7,
            ts_min: 4100,
            ts_max: 8191,
        };
        assert_eq!(SpillCell::decode(&c.encode()), c);
    }

    /// Everything handed over is readable after sync, at every level, and the
    /// coarser levels are exact merges of the finer one
    #[test]
    fn test_write_sync_read_all_levels() {
        let log = test_log();
        let cells: Vec<SpillCell> = (0..20_000u64)
            .map(|t| cell(t, ((t * 37) % 101) as f64))
            .collect();
        log.append(0, cells.clone());
        assert!(log.sync());

        let (level0, end0) = log.read(0, 0, 0, u64::MAX);
        assert_eq!(level0, cells);
        assert_eq!(end0, 20_000);

        // Level 1 holds complete 64 ms cells; the last partial one is not there yet
        let (level1, end1) = log.read(0, 1, 0, u64::MAX);
        assert_eq!(end1, 20_000 / 64 * 64);
        for c in &level1 {
            let mut expected = cells[c.ts as usize];
            for raw in &cells[c.ts as usize + 1..(c.ts + 64) as usize] {
                expected.absorb(raw);
            }
            assert_eq!(*c, expected);
        }
        assert_eq!(level1.len() as u64, end1 / 64);
    }

    /// A range read starts and stops at the right records (sparse index)
    #[test]
    fn test_range_read_uses_index_and_bounds() {
        let log = test_log();
        log.append(3, (0..50_000u64).map(|t| cell(t * 2, t as f64)).collect());
        assert!(log.sync());
        let (got, _) = log.read(3, 0, 30_001, 30_011);
        let times: Vec<u64> = got.iter().map(|c| c.ts).collect();
        assert_eq!(times, vec![30_002, 30_004, 30_006, 30_008, 30_010]);
        let (none, _) = log.read(7, 0, 0, 100);
        assert!(none.is_empty(), "unknown channel");
    }

    /// Dropping a generation removes its directory
    #[test]
    fn test_drop_removes_directory() {
        let log = test_log();
        log.append(0, vec![cell(1, 1.0)]);
        assert!(log.sync());
        let dir = log.dir().to_path_buf();
        assert!(dir.exists());
        drop(log);
        assert!(!dir.exists());
    }

    /// Past the byte cap nothing more is written and the cut is reported
    #[test]
    fn test_byte_cap_truncates_and_reports() {
        let base = std::env::temp_dir()
            .join("SerialMonitorEssential_test")
            .join(format!("spill-cap-{}", std::process::id()));
        let log = SpillLog::create(&base, 48 * 100).unwrap();
        log.append(0, (0..100u64).map(|t| cell(t, 0.0)).collect());
        log.append(0, (100..200u64).map(|t| cell(t, 0.0)).collect());
        assert!(log.sync());
        assert_eq!(log.truncated_from(), 100);
        let (got, end) = log.read(0, 0, 0, u64::MAX);
        assert_eq!(got.len(), 100);
        assert_eq!(end, 100);
    }
}
