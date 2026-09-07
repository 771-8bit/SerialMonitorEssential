# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

History before 0.1.0 is not reconstructed here; see the change log in
`docs/07_plotter_spec.md` and the git history for that period.

## [Unreleased]

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
