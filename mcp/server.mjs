#!/usr/bin/env node
/**
 * SerialMonitorEssential - MCP stdio server (layer 2 of the "AI port").
 *
 * Layer 1 (inside the Tauri app) exposes a local NDJSON TCP bridge on
 * 127.0.0.1:57320. This file adapts that bridge to the Model Context Protocol
 * so agents such as Claude Code can observe and drive the very same serial
 * session the human sees in the GUI (a COM port can only be opened once, so
 * the app acts as the multiplexer).
 *
 * The bridge client below is exported so it can be exercised without MCP
 * (see smoke.mjs). Server startup only happens when this file is the entry
 * point of the process.
 */

import net from 'node:net';
import { pathToFileURL } from 'node:url';

import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import { z } from 'zod';

export const SERVER_NAME = 'serial-monitor-essential';
export const SERVER_VERSION = '0.1.0';

export const DEFAULT_HOST = '127.0.0.1';
export const DEFAULT_PORT = 57320;
export const MAX_CHUNK_BYTES = 1048576;
export const DEFAULT_TAIL_BYTES = 4096;
export const REQUEST_TIMEOUT_MS = 5000;
export const WAIT_POLL_MS = 500;
/**
 * Lower bound for the tail window read on each `waitForPattern` poll. The
 * actual request grows to cover the backlog observed on the previous poll
 * (capped at MAX_CHUNK_BYTES). A fixed 64 KiB window only scans
 * 64 KiB / WAIT_POLL_MS = 131 KB/s, so above roughly 1 Mbps the device reply
 * scrolls out of the window between polls and is missed (docs/26 §5.1).
 */
export const WAIT_WINDOW_BYTES = 65536;
/**
 * Overlap retained after releasing an already-scanned prefix of the
 * accumulator. Keeps memory constant at high rates. A single match longer
 * than this may be missed at a trim boundary.
 */
export const WAIT_RETAIN_BYTES = 65536;
/** Above this ratio of non-printable bytes we render a hex dump instead of text. */
export const BINARY_RATIO = 0.1;
/**
 * Default cap on the characters a read tool returns.
 *
 * The budget is in CHARACTERS, not tokens: the server cannot know the client's
 * tokenizer, and any estimate is wrong for hex and CJK (docs/26). Roughly
 * 3.6 chars per token for ASCII logs.
 */
export const DEFAULT_MAX_CHARS = 4000;
/** The floor is above ENVELOPE_RESERVE: header and footer are never dropped. */
export const MIN_MAX_CHARS = 500;
export const MAX_MAX_CHARS = 200000;
/** Default budgets for the index-backed tools (the bridge owns the real defaults). */
export const DEFAULT_DIGEST_CHARS = 1800;
export const DEFAULT_WATCH_CHARS = 1200;
export const DEFAULT_SAMPLE_CHARS = 1400;

const MAX_RX_BUFFER = 8 * 1024 * 1024;

/* ------------------------------------------------------------------ */
/* Errors                                                              */
/* ------------------------------------------------------------------ */

export class BridgeError extends Error {
  constructor(message, { retryable = false, cause } = {}) {
    super(message);
    this.name = 'BridgeError';
    this.retryable = retryable;
    if (cause !== undefined) this.cause = cause;
  }
}

function unreachableMessage(host, port, detail) {
  const lines = [
    `シリアルブリッジに接続できません (${host}:${port})。`,
    'SerialMonitorEssential を起動し、設定画面で「AI Bridge」を ON にしてください。',
    `Cannot reach the SerialMonitorEssential bridge at ${host}:${port}.`,
    'Start the app and turn on "AI Bridge" in its settings.',
  ];
  if (detail) lines.push(`(${detail})`);
  return lines.join('\n');
}

function connectionLostMessage(host, port, detail) {
  const lines = [
    `シリアルブリッジとの接続が切れました (${host}:${port})。`,
    'アプリが終了したか、設定の「AI Bridge」が OFF になった可能性があります。',
    `Lost the connection to the SerialMonitorEssential bridge at ${host}:${port}.`,
    'The app may have quit, or "AI Bridge" may have been turned off.',
  ];
  if (detail) lines.push(`(${detail})`);
  return lines.join('\n');
}

function timeoutMessage(method, ms) {
  return [
    `ブリッジへの要求がタイムアウトしました (method=${method}, ${ms}ms)。`,
    `The bridge did not answer in time (method=${method}, ${ms}ms).`,
  ].join('\n');
}

function bridgeReturnedError(method, error) {
  return [
    `ブリッジがエラーを返しました (method=${method}): ${error}`,
    `The bridge returned an error (method=${method}): ${error}`,
  ].join('\n');
}

/* ------------------------------------------------------------------ */
/* Bridge client                                                       */
/* ------------------------------------------------------------------ */

/** Read connection settings from the environment (SME_BRIDGE_HOST/PORT/TOKEN). */
export function optionsFromEnv(env = process.env) {
  const port = Number.parseInt(env.SME_BRIDGE_PORT ?? '', 10);
  return {
    host: env.SME_BRIDGE_HOST || DEFAULT_HOST,
    port: Number.isFinite(port) && port > 0 ? port : DEFAULT_PORT,
    token: env.SME_BRIDGE_TOKEN || null,
  };
}

/**
 * One lazy TCP connection to the app's AI Bridge.
 * Sequential NDJSON request/response, per-request id, per-request timeout,
 * automatic reconnect after a transport level failure.
 */
export class BridgeClient {
  constructor(options = {}) {
    this.host = options.host ?? DEFAULT_HOST;
    this.port = Number(options.port ?? DEFAULT_PORT);
    this.token = options.token ?? null;
    this.timeoutMs = options.timeoutMs ?? REQUEST_TIMEOUT_MS;
    this.socket = null;
    this.connecting = null;
    this.pending = new Map();
    this.nextId = 1;
    this.rxBuffer = '';
    this.closed = false;
  }

  /** Send one request, connecting (and authenticating) on demand. */
  async request(method, params = {}) {
    for (let attempt = 0; attempt < 2; attempt++) {
      const socket = await this._connect();
      try {
        return await this._exchange(socket, method, params);
      } catch (err) {
        const retryable = err instanceof BridgeError && err.retryable;
        this._dropSocket();
        if (!retryable || attempt === 1) throw err;
      }
    }
    /* istanbul ignore next - unreachable, the loop always returns or throws */
    throw new BridgeError(unreachableMessage(this.host, this.port));
  }

  /** Close the connection (safe to call more than once). */
  close() {
    this.closed = true;
    this._failPending(
      new BridgeError(connectionLostMessage(this.host, this.port, 'client closed'))
    );
    this._dropSocket();
  }

  get connected() {
    return Boolean(this.socket) && !this.socket.destroyed;
  }

  _connect() {
    if (this.connected) return Promise.resolve(this.socket);
    if (this.connecting) return this.connecting;
    this.closed = false;
    const attempt = this._open();
    const guarded = attempt.finally(() => {
      if (this.connecting === guarded) this.connecting = null;
    });
    this.connecting = guarded;
    return guarded;
  }

  _open() {
    return new Promise((resolve, reject) => {
      const socket = net.createConnection({ host: this.host, port: this.port });
      socket.setNoDelay(true);

      const timer = setTimeout(() => {
        socket.destroy();
        reject(new BridgeError(unreachableMessage(this.host, this.port, 'connect timeout')));
      }, this.timeoutMs);

      const onError = (err) => {
        clearTimeout(timer);
        socket.destroy();
        reject(
          new BridgeError(unreachableMessage(this.host, this.port, err?.message), { cause: err })
        );
      };

      socket.once('error', onError);
      socket.once('connect', () => {
        clearTimeout(timer);
        socket.off('error', onError);
        socket.setEncoding('utf8');
        socket.on('data', (chunk) => this._onData(chunk));
        socket.on('error', (err) => this._onTransportFailure(err));
        socket.on('close', () => this._onTransportFailure(new Error('connection closed')));

        this.socket = socket;
        this.rxBuffer = '';

        if (!this.token) {
          resolve(socket);
          return;
        }
        this._exchange(socket, 'auth', { token: this.token }).then(
          () => resolve(socket),
          (err) => {
            this._dropSocket();
            reject(err);
          }
        );
      });
    });
  }

  _exchange(socket, method, params) {
    return new Promise((resolve, reject) => {
      if (!socket || socket.destroyed) {
        reject(
          new BridgeError(connectionLostMessage(this.host, this.port, 'socket is gone'), {
            retryable: true,
          })
        );
        return;
      }
      const id = this.nextId++;
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new BridgeError(timeoutMessage(method, this.timeoutMs)));
      }, this.timeoutMs);

      this.pending.set(id, { method, resolve, reject, timer });

      const line = `${JSON.stringify({ id, method, params: params ?? {} })}\n`;
      socket.write(line, 'utf8', (err) => {
        if (!err) return;
        const entry = this.pending.get(id);
        if (!entry) return;
        this.pending.delete(id);
        clearTimeout(entry.timer);
        reject(
          new BridgeError(connectionLostMessage(this.host, this.port, err.message), {
            retryable: true,
            cause: err,
          })
        );
      });
    });
  }

  _onData(chunk) {
    this.rxBuffer += chunk;
    if (this.rxBuffer.length > MAX_RX_BUFFER) {
      this._onTransportFailure(new Error('response buffer overflow'));
      return;
    }
    let index;
    while ((index = this.rxBuffer.indexOf('\n')) >= 0) {
      const line = this.rxBuffer.slice(0, index).trim();
      this.rxBuffer = this.rxBuffer.slice(index + 1);
      if (!line) continue;
      let message;
      try {
        message = JSON.parse(line);
      } catch {
        continue; // ignore garbage lines rather than killing the connection
      }
      // An id:null error line is a connection-level rejection (e.g. "too many
      // connections", sent just before the bridge closes the socket). Skipping
      // it would surface as a misleading "cannot reach the bridge" error.
      if (message && message.id === null && message.ok === false) {
        const reason = new BridgeError(
          bridgeReturnedError('connection', message.error ?? 'connection rejected')
        );
        this._failPending(reason);
        this._dropSocket();
        return;
      }
      const entry = this.pending.get(message?.id);
      if (!entry) continue;
      this.pending.delete(message.id);
      clearTimeout(entry.timer);
      if (message.ok) {
        entry.resolve(message.result ?? {});
      } else {
        entry.reject(
          new BridgeError(bridgeReturnedError(entry.method, message.error ?? 'unknown error'))
        );
      }
    }
  }

  _onTransportFailure(err) {
    if (!this.socket && this.pending.size === 0) return;
    this._dropSocket();
    if (this.closed) return;
    this._failPending(
      new BridgeError(connectionLostMessage(this.host, this.port, err?.message), {
        retryable: true,
        cause: err,
      })
    );
  }

  _failPending(error) {
    for (const entry of this.pending.values()) {
      clearTimeout(entry.timer);
      entry.reject(error);
    }
    this.pending.clear();
  }

  _dropSocket() {
    const socket = this.socket;
    this.socket = null;
    this.rxBuffer = '';
    if (socket) {
      socket.removeAllListeners();
      socket.on('error', () => {});
      socket.destroy();
    }
  }
}

/* ------------------------------------------------------------------ */
/* Rendering helpers                                                   */
/* ------------------------------------------------------------------ */

export function decodeBase64(base64) {
  return Buffer.from(base64 ?? '', 'base64');
}

function isValidUtf8(buf) {
  if (buf.length === 0) return true;
  const decoder = new TextDecoder('utf-8', { fatal: true });
  // A tail/window read can cut a multi-byte character at BOTH ends: skip up to
  // 3 leading continuation bytes (0b10xxxxxx) and allow up to 3 trailing bytes
  // to be a truncated sequence. Trailing-only tolerance misclassifies windows
  // that start mid-character, hex-dumping legitimate text.
  let start = 0;
  while (start < Math.min(3, buf.length) && (buf[start] & 0xc0) === 0x80) start++;
  const body = buf.subarray(start);
  if (body.length === 0) return true;
  for (let trim = 0; trim <= 3 && trim < body.length; trim++) {
    try {
      decoder.decode(body.subarray(0, body.length - trim));
      return true;
    } catch {
      /* keep trimming */
    }
  }
  return false;
}

/** Fraction of bytes that are neither printable ASCII, common whitespace, nor valid UTF-8 text. */
export function nonPrintableRatio(buf) {
  if (buf.length === 0) return 0;
  const utf8 = isValidUtf8(buf);
  let bad = 0;
  for (const byte of buf) {
    if (byte === 0x09 || byte === 0x0a || byte === 0x0d) continue;
    if (byte >= 0x20 && byte <= 0x7e) continue;
    if (byte >= 0x80 && utf8) continue;
    bad++;
  }
  return bad / buf.length;
}

export function looksLikeText(buf) {
  return nonPrintableRatio(buf) <= BINARY_RATIO;
}

/** Classic 16-bytes-per-row hex dump, offsets are absolute stream offsets. */
export function hexDump(buf, baseOffset = 0) {
  const rows = [];
  for (let i = 0; i < buf.length; i += 16) {
    const row = buf.subarray(i, i + 16);
    const cells = [];
    for (let j = 0; j < 16; j++) {
      cells.push(j < row.length ? row[j].toString(16).padStart(2, '0') : '  ');
    }
    const hex = `${cells.slice(0, 8).join(' ')}  ${cells.slice(8).join(' ')}`;
    let ascii = '';
    for (const byte of row) {
      ascii += byte >= 0x20 && byte <= 0x7e ? String.fromCharCode(byte) : '.';
    }
    rows.push(`${(baseOffset + i).toString(16).padStart(8, '0')}  ${hex}  |${ascii}|`);
  }
  return rows.join('\n');
}

/** Render bytes as lossy UTF-8 text, or as a hex dump when they look binary. */
export function renderBytes(buf, baseOffset = 0) {
  if (buf.length === 0) return { mode: 'empty', body: '(no data)' };
  if (looksLikeText(buf)) return { mode: 'text', body: buf.toString('utf8') };
  return { mode: 'hex', body: hexDump(buf, baseOffset) };
}

/**
 * Render a `find` reply as a compact answer.
 *
 * Counts and a few handles only — never bulk data. A zero is never bare: it
 * always carries the coverage that produced it, so "not found" cannot be
 * confused with "could not look" (docs/26 §6.2 rule 5).
 */
export function renderFind(res, maxChars = DEFAULT_MAX_CHARS) {
  const num = (k) => Number(res?.[k] ?? 0);
  const matches = num('matches');
  const exact = res?.exact === true;
  const complete = res?.complete === true;
  const start = num('range_start');
  const end = num('range_end');
  const scanned = num('bytes_scanned');
  const gap = num('gap_bytes');

  let qualifier;
  if (exact) {
    qualifier = 'exact';
  } else {
    const why = [];
    if (!complete) why.push('scan cut short');
    if (gap > 0) why.push('unreadable bytes skipped');
    qualifier = `AT LEAST — ${why.join(', ')}`;
  }
  const lines = [`matches: ${matches} (${qualifier})`];

  const requested = Math.max(0, end - start);
  const coverage = requested === 0 ? 100 : Math.min(100, Math.floor((scanned * 100) / requested));
  let cov = `scanned ${scanned} of ${requested} bytes in offset ${start}..${end} (${coverage}%)`;
  if (gap > 0) cov += `, ${gap} bytes unreadable`;
  cov += `, ${num('elapsed_ms')}ms`;
  lines.push(cov);

  if (matches > 0 && res?.first != null && res?.last != null) {
    lines.push(`first @off=${res.first}  last @off=${res.last}`);
  }
  if (!complete && end > start) {
    lines.push(`resume: serial_find(pattern=..., from=${start + scanned}, to=${end})`);
  }

  let body = lines.join('\n');
  const hits = Array.isArray(res?.hits) ? res.hits : [];
  let listed = 0;
  for (const h of hits) {
    const line = `\n@off=${h.offset}  ${h.excerpt ?? ''}${h.excerpt_truncated ? '…' : ''}`;
    if ([...body].length + [...line].length + 120 > maxChars) break;
    body += line;
    listed += 1;
  }
  if (matches > listed) {
    body +=
      `\nshowing ${listed} of ${matches} hits (raise max_hits, or narrow with from/to). ` +
      'Fetch context with serial_read_range(offset=<@off>, length=200).';
  } else if (listed > 0) {
    body += '\nall hits listed.';
  }

  const all = [...body];
  return all.length > maxChars ? all.slice(0, maxChars).join('') : body;
}

/** Parse the `format` tool argument. */
export function parseRenderFormat(v) {
  if (v === undefined || v === null) return 'auto';
  if (v === 'auto' || v === 'text' || v === 'hex' || v === 'lines') return v;
  throw new Error("bad arguments: 'format' must be one of auto/text/hex/lines");
}

/**
 * Number of leading bytes whose lossy-UTF-8 rendering fits in `maxChars`
 * characters. Counting characters (not bytes) means CJK logs are not
 * under-filled by 3x.
 */
export function headBytesWithinCharBudget(buf, maxChars) {
  let bytes = 0;
  let chars = 0;
  while (bytes < buf.length && chars < maxChars) {
    const b = buf[bytes];
    let width = 1;
    if (b >= 0xf0) width = 4;
    else if (b >= 0xe0) width = 3;
    else if (b >= 0xc0) width = 2;
    if (bytes + width > buf.length) width = 1;
    bytes += width;
    chars += 1;
  }
  return Math.min(bytes, buf.length);
}

/** Number of trailing bytes that fit, never splitting a multi-byte character. */
export function tailBytesWithinCharBudget(buf, maxChars) {
  let start = buf.length;
  let chars = 0;
  while (start > 0 && chars < maxChars) {
    let cand = start - 1;
    let steps = 0;
    while (cand > 0 && steps < 3 && (buf[cand] & 0xc0) === 0x80) {
      cand -= 1;
      steps += 1;
    }
    start = cand;
    chars += 1;
  }
  return buf.length - start;
}

/** Collapse consecutive identical lines into one line with [xN]. */
export function collapseRepeatedLines(text) {
  const out = [];
  let removed = 0;
  let prev = null;
  let run = 0;
  const flush = () => {
    if (prev === null) return;
    if (run > 1) {
      removed += run - 1;
      out.push(`${prev}  [x${run}]`);
    } else {
      out.push(prev);
    }
  };
  for (const line of text.split('\n')) {
    if (prev !== null && line === prev) {
      run += 1;
    } else {
      flush();
      prev = line;
      run = 1;
    }
  }
  flush();
  return { text: out.join('\n'), removed };
}

/** A hex dump row is 78 chars + newline, so chars map back to bytes exactly. */
export const HEX_ROW_CHARS = 79;
/** Characters held back for the header and the (never-dropped) footer. */
const ENVELOPE_RESERVE = 240;

/**
 * Render one chunk while strictly honouring `maxChars`.
 *
 * The budget is enforced by slicing BYTES before rendering, so the byte range
 * actually shown can be reported as absolute offsets and the rest fetched with
 * a single serial_read_range call (docs/26: handles, not payloads). Truncation
 * is always disclosed in a footer.
 */
export function renderChunk({
  buf,
  offset,
  totalBytes,
  extra = [],
  format = 'auto',
  collapse = false,
  maxChars = DEFAULT_MAX_CHARS,
  keepTail = true,
}) {
  const base = offset ?? 0;
  if (buf.length === 0) {
    const header = [
      `offset=${base}`,
      'bytes=0',
      totalBytes === undefined ? null : `total_bytes=${totalBytes}`,
      'rendering=empty',
      ...extra,
    ]
      .filter(Boolean)
      .join('  ');
    return { text: `${header}\n---\n(no data)`, shownStart: base, shownEnd: base, truncated: false };
  }

  const mode = format === 'auto' ? (looksLikeText(buf) ? 'text' : 'hex') : format;
  const bodyBudget = Math.max(80, maxChars - ENVELOPE_RESERVE);

  // --- slice by bytes: the one place the budget is enforced ---
  let slice;
  let shownStart;
  if (mode === 'hex') {
    const fit = Math.max(1, Math.floor(bodyBudget / HEX_ROW_CHARS)) * 16;
    if (buf.length <= fit) {
      slice = buf;
      shownStart = base;
    } else if (keepTail) {
      slice = buf.subarray(buf.length - fit);
      shownStart = base + buf.length - fit;
    } else {
      slice = buf.subarray(0, fit);
      shownStart = base;
    }
  } else if (keepTail) {
    const keep = tailBytesWithinCharBudget(buf, bodyBudget);
    slice = buf.subarray(buf.length - keep);
    shownStart = base + buf.length - keep;
  } else {
    slice = buf.subarray(0, headBytesWithinCharBudget(buf, bodyBudget));
    shownStart = base;
  }

  // `lines` drops the partial line at each end
  if (mode === 'lines') {
    const first = slice.indexOf(0x0a);
    if (first >= 0) {
      shownStart += first + 1;
      slice = slice.subarray(first + 1);
    } else {
      slice = slice.subarray(slice.length);
    }
    const last = slice.lastIndexOf(0x0a);
    slice = last >= 0 ? slice.subarray(0, last + 1) : slice.subarray(0, 0);
  }
  const shownEnd = shownStart + slice.length;
  const truncated = slice.length < buf.length;

  let body = mode === 'hex' ? hexDump(slice, shownStart) : slice.toString('utf8');
  let collapsed = 0;
  if (collapse && mode !== 'hex') {
    const c = collapseRepeatedLines(body);
    body = c.text;
    collapsed = c.removed;
  }

  const header = [
    `offset=${base}`,
    `bytes=${buf.length}`,
    totalBytes === undefined ? null : `total_bytes=${totalBytes}`,
    `rendering=${mode}`,
    ...extra,
    truncated ? `shown=${shownStart}..${shownEnd} (${slice.length} of ${buf.length} bytes)` : null,
    collapsed > 0 ? `collapsed=${collapsed} repeated lines` : null,
  ]
    .filter(Boolean)
    .join('  ');

  let foot = '';
  if (truncated) {
    const end = base + buf.length;
    const next = [];
    if (shownStart > base) next.push(`serial_read_range(offset=${base}, length=${shownStart - base})`);
    if (shownEnd < end) next.push(`serial_read_range(offset=${shownEnd}, length=${end - shownEnd})`);
    foot =
      `\n---\ntruncated: ${buf.length - slice.length} more bytes in this read ` +
      `(budget ${maxChars} chars). next: ${next.join(' and ')}`;
  }

  // Trim the BODY only; the footer is never dropped.
  const allowed = Math.max(0, maxChars - ([...header].length + 5 + [...foot].length));
  const bodyChars = [...body];
  if (bodyChars.length > allowed) body = bodyChars.slice(0, allowed).join('');

  // Backstop so exceeding the budget is structurally impossible.
  let text = `${header}\n---\n${body}${foot}`;
  const all = [...text];
  if (all.length > maxChars) text = all.slice(0, maxChars).join('');

  return { text, shownStart, shownEnd, truncated };
}

/* ------------------------------------------------------------------ */
/* Hex / line-ending helpers                                           */
/* ------------------------------------------------------------------ */

export const LINE_ENDINGS = { none: '', cr: '\r', lf: '\n', crlf: '\r\n' };

/** Accept "48 65 6c", "48:65:6C", "0x48 0x65", "4865 6c" ... -> Buffer. Throws on bad input. */
export function parseHex(input) {
  const cleaned = String(input ?? '')
    .replace(/0x/gi, '')
    .replace(/[\s,:;_-]/g, '');
  if (cleaned.length === 0) {
    throw new Error('hex が空です / hex string is empty');
  }
  if (!/^[0-9a-fA-F]+$/.test(cleaned)) {
    throw new Error(
      '16進数として不正な文字が含まれています / hex string contains non-hex characters'
    );
  }
  if (cleaned.length % 2 !== 0) {
    throw new Error(
      '16進数は2桁ずつ (バイト単位) で指定してください / hex string must have an even number of digits'
    );
  }
  return Buffer.from(cleaned, 'hex');
}

/* ------------------------------------------------------------------ */
/* wait_for                                                            */
/* ------------------------------------------------------------------ */

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * Poll the bridge tail until `pattern` matches data that arrived after the call
 * started. This is the "send a command, wait for the reply" primitive.
 *
 * Returns { matched, offset?, match?, excerpt?, tail?, elapsedMs, bytesScanned, totalBytes, gap }.
 */
export async function waitForPattern(client, options = {}) {
  const {
    pattern,
    timeoutMs = 10000,
    fromEnd = true,
    pollMs = WAIT_POLL_MS,
    windowBytes = WAIT_WINDOW_BYTES,
    backlogBytes = DEFAULT_TAIL_BYTES,
  } = options;

  const regex = new RegExp(pattern, 'm');
  const started = Date.now();
  const deadline = started + timeoutMs;

  const status = await client.request('status', {});
  const initialTotal = Number(status?.total_bytes ?? 0);
  let cursor = fromEnd ? initialTotal : Math.max(0, initialTotal - backlogBytes);

  let acc = Buffer.alloc(0);
  let accStart = cursor;
  let totalBytes = initialTotal;
  let gap = false;
  let scanned = 0;

  for (;;) {
    // Ask for everything that arrived since the last poll. The window grows
    // with the arrival rate, so a fast device cannot outrun it (docs/26 §5.1).
    const want = Math.min(
      MAX_CHUNK_BYTES,
      Math.max(windowBytes, totalBytes - cursor + windowBytes),
    );
    const res = await client.request('tail', { bytes: want });
    const newTotal = Number(res?.total_bytes ?? totalBytes);
    // Capture reset (port reopen / Clear): total_bytes rewound past our cursor.
    // Without this the cursor points past the new stream and every poll is
    // ignored, guaranteeing a timeout even though the reply arrived.
    if (newTotal < cursor) {
      acc = Buffer.alloc(0);
      accStart = 0;
      cursor = 0;
      gap = true;
    }
    totalBytes = newTotal;
    const chunk = decodeBase64(res?.base64);
    const chunkOffset = Number(res?.offset ?? 0);
    const chunkEnd = chunkOffset + chunk.length;

    if (chunkEnd > cursor) {
      if (chunkOffset > cursor) {
        // Data scrolled past our window between polls. REPLACE the accumulator:
        // concatenating across the discontinuity allows phantom matches that
        // span the gap and corrupts every offset computed afterwards.
        gap = true;
        scanned += chunk.length;
        acc = Buffer.from(chunk);
        accStart = chunkOffset;
        cursor = chunkEnd;
      } else {
        const take = chunk.subarray(cursor - chunkOffset);
        if (take.length > 0) {
          if (acc.length === 0) accStart = cursor;
          scanned += take.length;
          acc = Buffer.concat([acc, take]);
          cursor = chunkEnd;
        }
      }
    }

    const text = acc.toString('utf8');
    const match = regex.exec(text);
    if (match) {
      const byteIndex = Buffer.byteLength(text.slice(0, match.index), 'utf8');
      const from = Math.max(0, match.index - 80);
      const to = Math.min(text.length, match.index + match[0].length + 160);
      return {
        matched: true,
        offset: accStart + byteIndex,
        match: match[0],
        excerpt: text.slice(from, to),
        elapsedMs: Date.now() - started,
        bytesScanned: scanned,
        totalBytes,
        gap,
      };
    }

    const remaining = deadline - Date.now();
    if (remaining <= 0) {
      const tail = acc.subarray(Math.max(0, acc.length - 256));
      return {
        matched: false,
        tail,
        tailOffset: accStart + Math.max(0, acc.length - 256),
        elapsedMs: Date.now() - started,
        bytesScanned: scanned,
        totalBytes,
        gap,
      };
    }

    // Release the already-scanned prefix so memory stays constant at high
    // rates. accStart advances by the same amount, so reported offsets stay
    // absolute stream offsets. Not a gap: nothing unscanned was dropped.
    if (acc.length > WAIT_RETAIN_BYTES) {
      // Nudge the cut forward to a UTF-8 lead byte. This implementation matches
      // on the lossy-decoded string, so cutting mid-character would inject a
      // U+FFFD and skew byteIndex (DEBT-6 asymmetry 2). At most 3 steps for
      // valid UTF-8; capped so arbitrary binary cannot walk the buffer.
      let cut = acc.length - WAIT_RETAIN_BYTES;
      for (let i = 0; i < 3 && cut < acc.length && (acc[cut] & 0xc0) === 0x80; i += 1) cut += 1;
      accStart += cut;
      acc = acc.subarray(cut);
    }

    await sleep(Math.min(pollMs, remaining));
  }
}

/* ------------------------------------------------------------------ */
/* MCP wiring                                                          */
/* ------------------------------------------------------------------ */

function textResult(text) {
  return { content: [{ type: 'text', text }] };
}

function errorResult(err) {
  const text =
    err instanceof BridgeError ? err.message : `エラー / Error: ${err?.message ?? String(err)}`;
  return { content: [{ type: 'text', text }], isError: true };
}

/** Wrap a handler so every failure becomes an isError text result, never a throw. */
function handler(fn) {
  return async (args) => {
    try {
      return textResult(await fn(args ?? {}));
    } catch (err) {
      return errorResult(err);
    }
  };
}

/**
 * Call an index-backed bridge method and hand back its already-budgeted text.
 *
 * Adds nothing and removes nothing: if the bridge did not send text, say so
 * rather than returning an empty answer that reads like "nothing happened".
 */
export async function insightText(client, method, args) {
  const params = {};
  const keys = [
    'window', 'cursor', 'mode', 'template',
    'top', 'samples', 'buckets', 'max_chars', 'wait_ms', 'n', 'offset',
    'collapse_repeats',
  ];
  for (const key of keys) {
    if (args?.[key] !== undefined && args[key] !== null) params[key] = args[key];
  }
  const res = await client.request(method, params);
  const text = res?.text;
  if (typeof text !== 'string') throw new Error('bridge returned no text');
  return text;
}

export function createServer(client) {
  const server = new McpServer(
    { name: SERVER_NAME, version: SERVER_VERSION },
    {
      instructions: [
        'Debug a serial device through the running SerialMonitorEssential GUI app.',
        'The app owns the COM port; these tools read and write the same session the human sees.',
        'Typical flow: serial_digest to see what is going on, serial_watch (+ its cursor) to follow it, serial_send + serial_wait_for to interact.',
      ].join(' '),
    }
  );

  server.registerTool(
    'serial_status',
    {
      title: 'Serial status',
      description:
        'Report the bridge/app status: whether a serial port is open, its name, how many bytes have been received so far, and the app version.',
      inputSchema: {},
      annotations: { readOnlyHint: true, openWorldHint: true },
    },
    handler(async () => {
      const res = await client.request('status', {});
      const summary = [
        `connected: ${res.connected === true ? 'yes' : 'no'}`,
        `port: ${res.port_name ?? '(none)'}`,
        `total_bytes: ${res.total_bytes ?? 0}`,
        `app_version: ${res.app_version ?? 'unknown'} (protocol ${res.protocol ?? '?'})`,
      ].join('\n');
      // The old `raw: {full JSON}` repeated the four lines above verbatim:
      // zero extra information at double the tokens (docs/26 §5).
      return summary;
    })
  );

  server.registerTool(
    'serial_ports',
    {
      title: 'List serial ports',
      description: 'List the serial ports the app can see on this machine.',
      inputSchema: {},
      annotations: { readOnlyHint: true, openWorldHint: true },
    },
    handler(async () => {
      const res = await client.request('ports', {});
      const ports = Array.isArray(res) ? res : (res?.ports ?? []);
      if (!ports.length) return 'No serial ports found.';
      return `${ports.length} port(s):\n${ports.map((p) => `- ${p}`).join('\n')}`;
    })
  );

  // The index-backed tools. This adapter is deliberately a thin renderer: the
  // bridge's insight::render is the ONE place that applies the character budget
  // and words the report (docs/26 §2.3), so both adapters cannot drift apart.
  server.registerTool(
    'serial_digest',
    {
      title: 'Summarise the capture',
      description:
        "Understand a capture in ONE call at fixed cost: line templates with counts, alarm totals, rare and brand-new lines, plus a cursor for serial_watch. Reads the app's index, not the bulk data, so 4 KB and 4 GB cost the same. Prefer it over serial_read_tail to find out what is going on. Counts are exact unless the reply says otherwise, and every extreme carries a byte offset for serial_read_range.",
      inputSchema: {
        window: z
          .string()
          .optional()
          .describe(
            '"all" (default), a duration like "30s"/"5m", or "off:FROM..TO". A duration needs timestamps; without them the call fails rather than guessing.'
          ),
        template: z
          .string()
          .optional()
          .describe(
            'A template id like "t12" from a previous reply. Returns that one template in detail (each field range with offsets, plus real lines) instead of the overview.'
          ),
        top: z
          .number()
          .int()
          .min(1)
          .max(50)
          .optional()
          .describe('Frequent templates to list (default 8). Rare and new ones come first regardless.'),
        samples: z
          .number()
          .int()
          .min(0)
          .max(8)
          .optional()
          .describe('How many recent raw lines to quote (default 3).'),
        buckets: z
          .number()
          .int()
          .min(0)
          .max(24)
          .optional()
          .describe('Split the window into N buckets, reporting lines/err/warn each (default 0 = none).'),
        max_chars: z
          .number()
          .int()
          .min(MIN_MAX_CHARS)
          .max(MAX_MAX_CHARS)
          .optional()
          .describe(
            `Character cap (default ${DEFAULT_DIGEST_CHARS}). Alarms are never dropped to fit; samples are.`
          ),
      },
      annotations: { readOnlyHint: true, openWorldHint: true },
    },
    handler(async (args) => insightText(client, 'digest', args))
  );

  server.registerTool(
    'serial_watch',
    {
      title: 'Follow what changed',
      description:
        'Report only what changed since a cursor: new templates, per-template increases, and the alarm delta. Reply size does not depend on how much arrived, so following a 12 Mbps device costs the same as an idle one. Call it once without a cursor (or use serial_digest) to get one, then pass it back. A cursor from a cleared or reopened capture is not an error; the reply says so.',
      inputSchema: {
        cursor: z
          .string()
          .optional()
          .describe('Cursor from a previous reply. Omit on the first call to get one.'),
        wait_ms: z
          .number()
          .int()
          .min(0)
          .max(3000)
          .optional()
          .describe('Wait server-side this long for new data before answering (default 0).'),
        top: z
          .number()
          .int()
          .min(1)
          .max(50)
          .optional()
          .describe('Changed templates to list (default 8).'),
        max_chars: z
          .number()
          .int()
          .min(MIN_MAX_CHARS)
          .max(MAX_MAX_CHARS)
          .optional()
          .describe(`Character cap (default ${DEFAULT_WATCH_CHARS}).`),
      },
      annotations: { readOnlyHint: true, openWorldHint: true },
    },
    handler(async (args) => insightText(client, 'watch', args))
  );

  server.registerTool(
    'serial_sample',
    {
      title: 'Quote real lines',
      description:
        'Quote actual received lines when the summary is not enough, capped and with consecutive duplicates folded into [xN]. Every line comes with the offset it starts at, and lines cut off by the read window are dropped rather than shown as if they were whole. Use serial_digest first; reach for this when you need to see the bytes themselves.',
      inputSchema: {
        mode: z
          .enum(['tail', 'head', 'around', 'stratified', 'by_template'])
          .optional()
          .describe(
            'tail (default): newest lines. head: oldest indexed. around: needs offset. stratified: spread evenly over the capture. by_template: recent occurrences of one template.'
          ),
        n: z.number().int().min(1).max(50).optional().describe('How many lines (default 10).'),
        offset: z.number().int().min(0).optional().describe('Centre offset for mode=around.'),
        template: z.string().optional().describe('Template id like "t12" for mode=by_template.'),
        collapse_repeats: z
          .boolean()
          .optional()
          .describe('Fold consecutive identical lines into [xN] (default true). Counts stay exact.'),
        max_chars: z
          .number()
          .int()
          .min(MIN_MAX_CHARS)
          .max(MAX_MAX_CHARS)
          .optional()
          .describe(`Character cap (default ${DEFAULT_SAMPLE_CHARS}).`),
      },
      annotations: { readOnlyHint: true, openWorldHint: true },
    },
    handler(async (args) => insightText(client, 'sample', args))
  );

  server.registerTool(
    'serial_read_tail',
    {
      title: 'Read latest serial data',
      description:
        `Read the most recent bytes received from the device. Rendered as UTF-8 text, or as a hex dump when the data looks binary. Default ${DEFAULT_TAIL_BYTES} bytes, max ${MAX_CHUNK_BYTES}. Output is capped at max_chars (${DEFAULT_MAX_CHARS} by default); when it does not fit, the most recent part is kept and a footer reports the exact byte range shown plus a serial_read_range call for the rest.`,
      inputSchema: {
        bytes: z
          .number()
          .int()
          .min(1)
          .max(MAX_CHUNK_BYTES)
          .optional()
          .describe(`How many trailing bytes to read (default ${DEFAULT_TAIL_BYTES}).`),
        format: z
          .enum(['auto', 'text', 'hex', 'lines'])
          .optional()
          .describe(
            'auto (default): text, or hex when the data looks binary. text: always UTF-8. hex: always a hex dump (about 79 chars per 16 bytes, so it burns budget ~5x faster than text). lines: text with the partial first and last lines dropped.'
          ),
        collapse_repeats: z
          .boolean()
          .optional()
          .describe(
            'Collapse consecutive identical lines into one line with [xN]. Applied after the byte budget, so raise "bytes" as well when a log is highly repetitive. Not available in hex.'
          ),
        max_chars: z
          .number()
          .int()
          .min(MIN_MAX_CHARS)
          .max(MAX_MAX_CHARS)
          .optional()
          .describe(
            `Hard cap on the returned characters (default ${DEFAULT_MAX_CHARS}). Roughly 3.6 chars per token for ASCII logs.`
          ),
      },
      annotations: { readOnlyHint: true, openWorldHint: true },
    },
    handler(async ({ bytes, format, collapse_repeats: collapseRepeats, max_chars: maxChars }) => {
      const want = bytes ?? DEFAULT_TAIL_BYTES;
      const res = await client.request('tail', { bytes: want });
      const buf = decodeBase64(res?.base64);
      // tail means "the latest", so keep the end when the budget is tight.
      return renderChunk({
        buf,
        offset: Number(res?.offset ?? 0),
        totalBytes: Number(res?.total_bytes ?? 0),
        format: parseRenderFormat(format),
        collapse: collapseRepeats === true,
        maxChars: maxChars ?? DEFAULT_MAX_CHARS,
        keepTail: true,
      }).text;
    })
  );

  server.registerTool(
    'serial_read_range',
    {
      title: 'Read a serial byte range',
      description:
        `Read an explicit byte range of the received stream (offsets match those reported by serial_read_tail and serial_wait_for). Rendered as text, or as a hex dump when the data looks binary. Output is capped at max_chars (${DEFAULT_MAX_CHARS} by default); when it does not fit, the start of the range is kept and a footer gives the exact offset to resume from.`,
      inputSchema: {
        offset: z.number().int().min(0).describe('Absolute start offset in the received stream.'),
        length: z
          .number()
          .int()
          .min(1)
          .max(MAX_CHUNK_BYTES)
          .describe(`How many bytes to read (max ${MAX_CHUNK_BYTES}).`),
        format: z
          .enum(['auto', 'text', 'hex', 'lines'])
          .optional()
          .describe(
            'auto (default): text, or hex when the data looks binary. text: always UTF-8. hex: always a hex dump (about 79 chars per 16 bytes). lines: text with the partial first and last lines dropped.'
          ),
        collapse_repeats: z
          .boolean()
          .optional()
          .describe(
            'Collapse consecutive identical lines into one line with [xN]. Not available in hex.'
          ),
        max_chars: z
          .number()
          .int()
          .min(MIN_MAX_CHARS)
          .max(MAX_MAX_CHARS)
          .optional()
          .describe(`Hard cap on the returned characters (default ${DEFAULT_MAX_CHARS}).`),
      },
      annotations: { readOnlyHint: true, openWorldHint: true },
    },
    handler(
      async ({ offset, length, format, collapse_repeats: collapseRepeats, max_chars: maxChars }) => {
        const res = await client.request('read_range', { offset, length });
        const buf = decodeBase64(res?.base64);
        // A range read asks "from here", so keep the start when it is tight.
        return renderChunk({
          buf,
          offset: Number(res?.offset ?? offset),
          extra: [`length_read=${res?.length_read ?? buf.length}`],
          format: parseRenderFormat(format),
          collapse: collapseRepeats === true,
          maxChars: maxChars ?? DEFAULT_MAX_CHARS,
          keepTail: false,
        }).text;
      }
    )
  );

  server.registerTool(
    'serial_find',
    {
      title: 'Search the capture',
      description:
        'Search the whole capture for a regular expression, server-side, and get back the number of matches plus a few byte offsets. Use this INSTEAD of reading bulk data to look for something: it costs the same handful of tokens whether the capture is 4 KB or 4 GB, and it works past the 1 MiB read limit. Then fetch context for one interesting offset with serial_read_range. The match count is exact unless the reply says AT LEAST (the scan was cut short or bytes were unreadable), and a zero always comes with the coverage that produced it.',
      inputSchema: {
        pattern: z
          .string()
          .describe('Regular expression (matched in multi-line mode over the raw stream).'),
        from: z.number().int().min(0).optional().describe('Start offset of the search range (default 0).'),
        to: z
          .number()
          .int()
          .min(0)
          .optional()
          .describe('End offset of the search range (default: end of the capture).'),
        max_hits: z
          .number()
          .int()
          .min(0)
          .max(200)
          .optional()
          .describe(
            'How many matching lines to quote (default 20). The count is always complete regardless of this.'
          ),
        count_only: z
          .boolean()
          .optional()
          .describe('Return only the count and coverage, quoting no lines. The cheapest answer.'),
        max_chars: z
          .number()
          .int()
          .min(MIN_MAX_CHARS)
          .max(MAX_MAX_CHARS)
          .optional()
          .describe(`Hard cap on the returned characters (default ${DEFAULT_MAX_CHARS}).`),
      },
      annotations: { readOnlyHint: true, openWorldHint: true },
    },
    handler(
      async ({ pattern, from, to, max_hits: maxHits, count_only: countOnly, max_chars: maxChars }) => {
        const params = { pattern, max_hits: countOnly ? 0 : (maxHits ?? 20) };
        if (from !== undefined) params.from = from;
        if (to !== undefined) params.to = to;
        const res = await client.request('find', params);
        return renderFind(res, maxChars ?? DEFAULT_MAX_CHARS);
      }
    )
  );

  server.registerTool(
    'serial_send',
    {
      title: 'Send text to the device',
      description:
        'Send a text command to the serial device, with an optional line ending (default "lf"). The GUI shows every bridge send to the human, so the operator always sees what the agent transmitted.',
      inputSchema: {
        text: z.string().describe('Text to transmit (the line ending is added by the app).'),
        line_ending: z
          .enum(['none', 'cr', 'lf', 'crlf'])
          .optional()
          .describe('Line ending appended to the text. Default "lf".'),
      },
      annotations: { readOnlyHint: false, destructiveHint: false, openWorldHint: true },
    },
    handler(async ({ text, line_ending }) => {
      const ending = line_ending ?? 'lf';
      const res = await client.request('send', { text, line_ending: ending });
      return `Sent ${JSON.stringify(text)} + ${ending} -> bytes_written=${res?.bytes_written ?? 0}`;
    })
  );

  server.registerTool(
    'serial_send_hex',
    {
      title: 'Send raw bytes to the device',
      description:
        'Send raw bytes given as hex (e.g. "01 03 00 00 00 0A" or "0103000000 0A"). Separators, colons and 0x prefixes are ignored; the digit count must be even. Nothing is appended. The GUI shows every bridge send to the human.',
      inputSchema: {
        hex: z.string().describe('Hex byte string, e.g. "DE AD BE EF".'),
      },
      annotations: { readOnlyHint: false, destructiveHint: false, openWorldHint: true },
    },
    handler(async ({ hex }) => {
      const buf = parseHex(hex);
      const res = await client.request('send', { base64: buf.toString('base64') });
      return `Sent ${buf.length} byte(s) [${buf.toString('hex').replace(/(..)(?=.)/g, '$1 ')}] -> bytes_written=${res?.bytes_written ?? 0}`;
    })
  );

  server.registerTool(
    'serial_wait_for',
    {
      title: 'Wait for a pattern',
      description:
        'Wait until a JavaScript regular expression matches data received after this call started (polls every 500ms). Use it right after serial_send to capture the device reply. Returns the matching excerpt and its stream offset, or a timeout report with the last 256 bytes seen.',
      inputSchema: {
        pattern: z
          .string()
          .describe('JavaScript regular expression source, matched with the "m" flag.'),
        timeout_ms: z
          .number()
          .int()
          .min(100)
          .max(600000)
          .optional()
          .describe('How long to wait, in milliseconds. Default 10000.'),
        from_end: z
          .boolean()
          .optional()
          .describe(
            'true (default): only match data that arrives after this call. false: also search the last 4096 bytes already buffered.'
          ),
      },
      annotations: { readOnlyHint: true, openWorldHint: true },
    },
    handler(async ({ pattern, timeout_ms, from_end }) => {
      const result = await waitForPattern(client, {
        pattern,
        timeoutMs: timeout_ms ?? 10000,
        fromEnd: from_end ?? true,
      });
      if (result.matched) {
        const head = [
          'MATCH',
          `pattern=${JSON.stringify(pattern)}`,
          `offset=${result.offset}`,
          `elapsed_ms=${result.elapsedMs}`,
          `total_bytes=${result.totalBytes}`,
          result.gap ? 'warning=data_gap' : null,
        ]
          .filter(Boolean)
          .join('  ');
        return `${head}\n---\n${result.excerpt}`;
      }
      const rendered = renderBytes(result.tail, result.tailOffset);
      const head = [
        'TIMEOUT',
        `pattern=${JSON.stringify(pattern)}`,
        `elapsed_ms=${result.elapsedMs}`,
        `bytes_scanned=${result.bytesScanned}`,
        `total_bytes=${result.totalBytes}`,
      ].join('  ');
      const tailLabel =
        result.bytesScanned === 0
          ? 'no new data arrived while waiting'
          : `last ${result.tail.length} byte(s) seen, from offset ${result.tailOffset}`;
      return `${head}\n--- ${tailLabel}\n${rendered.body}`;
    })
  );

  return server;
}

export async function main() {
  const client = new BridgeClient(optionsFromEnv());
  const server = createServer(client);
  const transport = new StdioServerTransport();

  server.server.onclose = () => {
    client.close();
  };

  const shutdown = () => {
    client.close();
    server.close().catch(() => {});
  };
  process.once('SIGINT', () => {
    shutdown();
    process.exit(0);
  });
  process.once('SIGTERM', () => {
    shutdown();
    process.exit(0);
  });

  await server.connect(transport);
  console.error(
    `[${SERVER_NAME}] MCP stdio server ready (bridge ${client.host}:${client.port}${client.token ? ', token set' : ''})`
  );
}

const isEntryPoint = (() => {
  const arg = process.argv[1];
  if (!arg) return false;
  try {
    return import.meta.url === pathToFileURL(arg).href;
  } catch {
    return false;
  }
})();

if (isEntryPoint) {
  main().catch((err) => {
    console.error(`[${SERVER_NAME}] fatal:`, err);
    process.exit(1);
  });
}
