# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

History before 0.1.0 is not reconstructed here; see the change log in
`docs/07_plotter_spec.md` and the git history for that period.

## [Unreleased]

### Added

- **Auto Reconnect** (the former placeholder "Auto Connect" checkbox): when the device goes away
  (unplugged or reset) and comes back on the same port, the app reopens it and keeps appending
  to the same capture, so the output from before the reset is kept. Off by default; the setting
  is remembered. The bridge `status` reports `reconnecting: true` while waiting, and the MCP
  `serial_status` tool says so.

### Changed

- Plotter: zooming into any past range (Inspect / Paused) now shows the data at full resolution,
  however old it is. The plotter writes every 1 ms cell to temporary files (deleted on Clear and
  on exit; capped at 4 GiB per session). State changes older than the 10,000 kept in memory are
  written there too instead of being dropped.
- Plotter: samples are now stored in time-aligned tiers (fine for recent data, coarser for
  older data, boundaries never move). Before, after about 10,000 samples per channel the whole
  history was re-binned periodically, so even the default 10 s window became coarse every 50 s
  at 100 Hz. A live window is now drawn from the samples in it alone, and memory per channel
  stays bounded.
- Plotter: the default downsampling mode **LTTB is replaced by Peak**, which draws the minimum and
  maximum sample of every pixel column at their real times. LTTB drew bucket averages, so a
  single-sample spike of 400 was drawn as about 41 in a 10 s window and smaller spikes vanished in
  a 300 s window. Incoming samples are also no longer averaged in groups of 10 before storage, so
  slow streams are drawn sample for sample and already-drawn points no longer change.
- The settings toggle formerly labelled **AI Bridge** is now **MCP Server** (same feature,
  same port and protocol; `SME_BRIDGE_*` environment variables are unchanged).
- All user-facing text is English only: the remaining Japanese UI strings (bridge tooltip,
  connection/send status, log-error alert) were translated, and MCP error messages are no
  longer bilingual.

### Fixed

- Plotter: connecting to a device that is already streaming no longer creates a phantom channel
  from the cut first line (e.g. a state channel named `cos` with the value `accel:0.049`). The same
  applies where reception resumes after an automatic reconnect.

- Plotter: the right edge of the LIVE window crept ahead of real time (about 3 s after 3 minutes),
  so after roughly 10 minutes a 10 s window showed no data at all.

- Sending no longer stops reception. Previously a send held the port lock for the
  whole write, so data arriving meanwhile overflowed the driver buffer: on a real
  12 Mbaud UART (FT232H) a 16 KiB send lost 60 % of the incoming data, and with
  RTS/CTS flow control the app could deadlock itself until the write timed out.
  The receive worker now owns the port and interleaves 1 ms write slices with reads
  (ADR-14). Verified: 12 Mbaud, RTS/CTS, AI Bridge on, 64 KiB sends for 60 s —
  65 MB with zero loss.
- A send now fails only if no byte is accepted for 3 s (it reports how many bytes
  went out), instead of after a fixed time budget that hardware flow control could
  exceed.
- Clearing the capture no longer aborts a send in progress.

### Changed

- Sending from the GUI no longer blocks the window while the bytes go out.
- AI Bridge `send`: `offset_before` is now the position when the app started writing
  (data that arrived while the send was queued is no longer scanned as the reply),
  and `status.connected` turns false as soon as the device disconnects.

## [0.1.0] - Unreleased (first Tauri/Rust release)

First release of the Tauri/Rust rewrite (succeeding the earlier C# version).

### Added

- Serial monitor with Hex / ASCII views, virtual scrolling, timestamps, line
  wrap and control-character rendering (SYS-F-3xx).
- High-speed reception with chunk-based memory management and disk spill, so the
  full capture is browsable without an in-memory size limit (SYS-NF-101/103/104).
- TX as text or hex, selectable line ending (None/CR/LF/CRLF), input history,
  DTR/RTS control, binary export, copy as hex/ASCII.
- Real-time plotter: oscilloscope-style sliding window (1 s – 300 s) with
  absolute-time-aligned downsampling (LTTB, or average with min/max bands), a
  state timeline for discrete values, and LIVE / Inspect / Paused view states
  (SYS-F-5xx, INV-7).
- Active hotplug detection: port-list polling (2 s) plus reception-thread error
  detection for disconnects (SYS-F-107).
- AI integration (MCP): a localhost-only NDJSON bridge inside the app
  (`127.0.0.1:57320`, off by default) that lets an AI agent read the same
  capture and send to the same port the human is watching; every agent send is
  shown in the GUI (SYS-F-1101–1106).
- Built-in MCP stdio adapter (`--mcp`) so AI integration needs no Node.js or repo
  checkout, plus an in-app Setup Guide window that shows the exact registration
  command for the installed executable (SYS-F-1107–1109).
- AI integration: `serial_digest` and `serial_watch`, answered from an index the
  app keeps while the bridge is on. One call summarises a whole capture (line
  templates with counts, alarm totals, rare and brand-new lines) and a cursor
  follows it; both replies stay the same size whether the device sent 4 KB or
  4 GB. Counts come with the coverage that produced them, every reported extreme
  carries a byte offset, and a cursor from a cleared capture is answered with
  "generation changed" rather than a wrong delta (docs/26 stage 2).
- AI integration: waiting for a device reply is now scanned by the app itself.
  `serial_send(expect=...)` sends and waits in one call, starting the scan at the
  byte offset from just before the write, so a reply that lands immediately
  cannot be missed. The old client-side polling could only scan ~131 KB/s and
  lost replies above roughly 1 Mbps (docs/26 stage 4, resolves DEBT-8).
- AI integration: when the captured stream does not look line oriented, `serial_digest`
  says so instead of presenting line counts and templates that mean nothing
  (docs/26 stage 5, partial).
- AI integration: `serial_sample` quotes real received lines, capped and with
  consecutive duplicates folded into `[xN]` (exact counts), each carrying the
  offset it starts at; lines cut off by the read window are dropped rather than
  shown as if they were whole. `serial_digest(template="t12")` returns one
  template in detail: every field's range with the offsets where the extremes
  occurred, plus the real lines behind it (docs/26 stage 3).
- AI integration: `serial_find` searches the whole capture server-side and
  returns match counts plus offsets instead of bulk data, so it works far past
  the 1 MiB read limit (docs/26 stage 1).
- AI integration: read tools enforce a character budget (`max_chars`, `format`,
  `collapse_repeats`); a 4096-byte binary read went from 20,223 to 3,902
  characters, and what was left out is named as an exact `serial_read_range`
  call (docs/26 stage 0).
- Windows (NSIS per-user, MSI per-machine) and Linux (deb, AppImage) packaging;
  side-by-side coexistence with the C# version.

### Notes

- Data integrity verified on real hardware (Raspberry Pi Pico, USB-CDC):
  zero byte loss with matching SHA-256 at ~2 Mbps sustained. True 12 Mbps
  line-rate acceptance is pending a High-Speed USB-serial jig (SYS-NF-101).
