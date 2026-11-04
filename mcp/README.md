# SerialMonitorEssential MCP Server

An MCP (Model Context Protocol) stdio server that lets an AI agent (such as Claude Code)
**read and write the serial session that the SerialMonitorEssential GUI app already has open**.

> **If you use the installed app, this directory is not needed.**
> The same MCP adapter is **built into the app itself** and starts with
> `serial-monitor-essential --mcp`, no Node.js required (the app's **Settings → Setup Guide**
> shows the registration command with the actual exe path filled in). The Node implementation
> in this directory is a **reference implementation for development**; tools are added or
> changed together with the Rust version (`src-tauri/src/mcp_stdio.rs`)
> (docs/22 ADR-13 / DEBT-6, Japanese). The only difference is the regex dialect
> (Node version = JavaScript regex; built-in version = Rust regex, no lookaround support).

## Purpose

A COM port is an exclusive device: only one process can have it open at a time.
In this setup, **the app itself acts as the multiplexer**.

```
  Device ── COM port ── SerialMonitorEssential (the app is the sole owner)
                              ├── GUI ............ what a human watches and sends from
                              └── bridge (TCP) ──── MCP adapter ── AI agent
```

Humans use the GUI, the AI uses MCP. Both see **the same receive buffer and the same port**,
so workflows like "let the AI inspect the log you are watching" or "have the AI type commands
while you verify on screen" work without fighting over the port.

| Layer | What it is | Role |
| --- | --- | --- |
| Layer 1 | Bridge inside the app (the "MCP Server" toggle in Settings) | Runs an NDJSON TCP server on `127.0.0.1:57320` |
| Layer 2 | This directory (`server.mjs`) | A stdio server that translates that TCP into MCP tools |

## Prerequisites

1. **SerialMonitorEssential must be running.**
2. **Turn on "MCP Server" in the settings panel** (off by default).
3. Node.js **20 or later**.
4. Install dependencies:

   ```bash
   cd mcp
   npm install
   ```

If the app is not running or MCP Server is off, the tools do not throw; they return an
error message telling you to start the app and turn on
MCP Server in its settings.

## Registering with Claude Code

```bash
claude mcp add serial-monitor -- node "C:/Users/kazuki/Documents/SerialMonitorEssential/mcp/server.mjs"
```

To pass environment variables, use `-e`.

```bash
claude mcp add serial-monitor -e SME_BRIDGE_PORT=57320 -- node "C:/Users/kazuki/Documents/SerialMonitorEssential/mcp/server.mjs"
```

To write it into `.mcp.json` directly:

```json
{
  "mcpServers": {
    "serial-monitor": {
      "command": "node",
      "args": ["C:/Users/kazuki/Documents/SerialMonitorEssential/mcp/server.mjs"],
      "env": {}
    }
  }
}
```

After registering, you should see `serial-monitor` in `claude mcp list`.

## Environment variables

| Variable | Default | Description |
| --- | --- | --- |
| `SME_BRIDGE_HOST` | `127.0.0.1` | Bridge host. Anything other than loopback is not recommended. |
| `SME_BRIDGE_PORT` | `57320` | Bridge port. Must match the setting in the app. |
| `SME_BRIDGE_TOKEN` | (none) | Only needed if a token is configured on the app side. `auth` is sent automatically right after connecting. |

## Tools

| Tool | Arguments | Returns |
| --- | --- | --- |
| `serial_status` | none | Connection state, port name, received byte count, app version (four lines; the duplicate raw JSON dump was removed) |
| `serial_ports` | none | List of serial ports the PC recognizes |
| `serial_digest` | `window?` (`all` / `30s` / `off:A..B`), `template?` (`"t12"` = detail of one template), `top?`, `samples?`, `buckets?`, `max_chars?` | One-call summary from the app's index: line templates with counts, alarm totals, rare and brand-new lines, optional per-bucket timeline, and a cursor for `serial_watch`. Reads no bulk data, so the reply size does not follow the arrival rate |
| `serial_sample` | `mode?` (`tail`/`head`/`around`/`stratified`/`by_template`), `n?` (default 10), `offset?`, `template?`, `collapse_repeats?`, `max_chars?` | Real received lines, capped, each with the offset it starts at. Consecutive duplicates fold into `[xN]` with an exact count. Lines cut off by the read window are dropped rather than shown as if they were whole |
| `serial_watch` | `cursor?`, `wait_ms?` (max 3000), `top?`, `max_chars?` | Only what changed since the cursor. A cursor from a cleared or reopened capture is answered with "generation changed", never with a plausible-looking wrong delta |
| `serial_read_tail` | `bytes?` (default 4096 / max 1048576), `format?`, `collapse_repeats?`, `max_chars?` | The most recent received data. Text is shown as-is; data that looks binary is shown as a 16-bytes-per-line hex dump. Includes offset and total_bytes. Output is capped at `max_chars` (default 4000) |
| `serial_read_range` | `offset`, `length` (max 1048576), `format?`, `collapse_repeats?`, `max_chars?` | Received data in the given range. Same display and budget rules as `serial_read_tail` |
| `serial_find` | `pattern`, `from?`, `to?`, `max_hits?` (default 20), `count_only?`, `max_chars?` | Searches the whole capture server-side and returns match counts plus a few offsets, never bulk data. Costs the same whether the capture is 4 KB or 4 GB, and is not limited to 1 MiB |
| `serial_send` | `text`, `line_ending?` (`none`/`cr`/`lf`/`crlf`, default `lf`), `expect?`, `expect_timeout_ms?` | Number of bytes written. With `expect`, also waits for the reply in the same call, scanning from the offset just before the write. **The send is also shown in the GUI** |
| `serial_send_hex` | `hex` (e.g. `"01 03 00 00 00 0A"`), `expect?`, `expect_timeout_ms?` | Validates the hex pairs and sends the raw bytes. No line ending is appended. `expect` works as in `serial_send` |
| `serial_wait_for` | `pattern`, `timeout_ms?` (default 10000), `from_end?` (default true) | Waits for a regex over the data that arrives after the call. The app scans the capture forward itself, so nothing is missed however fast the device talks. Returns the matching portion and its offset; on timeout, the last 256 bytes seen. Prefer `serial_send` with `expect` when waiting for a reply to something you just sent |

Notes:

- Reading tools enforce a character budget. `format` is `auto` (default), `text`, `hex` or
  `lines`; `collapse_repeats` folds consecutive identical lines into `[xN]`; `max_chars`
  (default 4000, range 500..200000) is a hard cap. When the output does not fit, the bytes are
  sliced before rendering, the header reports `shown=<start>..<end>`, and a footer gives the
  exact `serial_read_range(...)` call for what was left out.
- `serial_find` reports an exact count, or `AT LEAST` when the scan was cut short (3 s budget)
  or skipped unreadable bytes. A zero always comes with the coverage that produced it, so
  "not found" cannot be mistaken for "could not look".
- `serial_digest` / `serial_watch` need the index, which runs only while MCP Server is on.
  Turning the bridge on does not index what arrived before it: the reply states the coverage,
  and any lag or unreadable region is reported rather than hidden. A count of zero always
  comes with the coverage that produced it.
- If more than 5% of the indexed bytes are control bytes, the reply says the stream does
  not look line oriented and that the line counts and templates are not meaningful
  (bytes >= 0x80 count as printable, so CJK logs are not mistaken for binary).
- The template table is bounded. When it overflows, the template not seen for the longest time
  is dropped and the reply says how many templates and lines were evicted, so the top-N and
  the total never quietly disagree.
- The Node adapter is a thin renderer for these two: the character budget and the wording are
  applied once, in the app's `insight::render`, so both adapters cannot drift apart.
- `serial_wait_for`'s `pattern` is a JavaScript regular expression source (evaluated with the `m` flag).
- The match offset is decided by the app on raw bytes, so both adapters report the same
  value even when invalid UTF-8 appears before the match.
- With `from_end: false`, the search also includes the most recent 4096 bytes already in the buffer.
- Binary detection is "more than 10% non-printable bytes". Japanese logs that are valid UTF-8
  are treated as text.

## Typical flow

```
1. serial_digest            → what is this device doing? (one call, fixed cost)
2. serial_watch (+ cursor)  → follow it; only the changes come back
   serial_digest(template=)  → one template in detail; serial_sample → the real lines
3. serial_send(expect=)     → send "AT+VER" and get the reply in the same call
4. serial_sample / serial_read_range → look at the exact lines when you need them

To investigate a long capture, search instead of reading:

1. serial_find              → e.g. "E:|PANIC|timeout" → counts, plus the first and last offsets
2. serial_read_range        → pull ~200 bytes around one interesting offset
```

Example instruction to an agent:

> Check the port with `serial_status`, send `AT+VER` with `serial_send`,
> wait 5 seconds for `VER:.*` with `serial_wait_for`, and tell me the version that comes back.

When you want to re-read a narrower range, the `offset` returned by `serial_wait_for` can be
passed straight to `serial_read_range`.

## Security

- **Listens on `127.0.0.1` only.** Not reachable from external networks.
- **Off by default.** Runs only when MCP Server is explicitly turned on in the app's settings.
- **Every send is shown in the GUI.** A human can see on screen what the AI wrote.
- Token authentication can be enabled with `SME_BRIDGE_TOKEN` if needed (against other
  processes on the same PC).
- The MCP server itself reads and writes no files. Standard output is reserved for the MCP
  protocol; all logging goes to standard error.

## Development and testing

```bash
node --check server.mjs   # syntax check
node smoke.mjs            # integration test against a fake bridge (prints PASS lines and exits 0)
npm start                 # run the MCP server standalone (exits on stdin EOF)
```

`smoke.mjs` does not use MCP; it starts a **fake bridge** server on an ephemeral port and
exercises `server.mjs`'s bridge client (`BridgeClient` / `waitForPattern` / the rendering
functions). It covers the status round trip, base64 decoding, sends with line endings,
`serial_wait_for`'s polling match, and the immediate failure — including the message
content — when the app is not running.
