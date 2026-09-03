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
export const WAIT_WINDOW_BYTES = 65536;
/** Above this ratio of non-printable bytes we render a hex dump instead of text. */
export const BINARY_RATIO = 0.1;

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

function renderChunk({ buf, offset, totalBytes, extra = [] }) {
  const rendered = renderBytes(buf, offset ?? 0);
  const header = [
    `offset=${offset ?? 0}`,
    `bytes=${buf.length}`,
    totalBytes === undefined ? null : `total_bytes=${totalBytes}`,
    `rendering=${rendered.mode}`,
    ...extra,
  ]
    .filter(Boolean)
    .join('  ');
  return `${header}\n---\n${rendered.body}`;
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

  for (;;) {
    const res = await client.request('tail', { bytes: windowBytes });
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
        acc = Buffer.from(chunk);
        accStart = chunkOffset;
        cursor = chunkEnd;
      } else {
        const take = chunk.subarray(cursor - chunkOffset);
        if (take.length > 0) {
          if (acc.length === 0) accStart = cursor;
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
        bytesScanned: acc.length,
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
        bytesScanned: acc.length,
        totalBytes,
        gap,
      };
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

export function createServer(client) {
  const server = new McpServer(
    { name: SERVER_NAME, version: SERVER_VERSION },
    {
      instructions: [
        'Debug a serial device through the running SerialMonitorEssential GUI app.',
        'The app owns the COM port; these tools read and write the same session the human sees.',
        'Typical flow: serial_status -> serial_send -> serial_wait_for -> serial_read_tail.',
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
      return `${summary}\n\nraw: ${JSON.stringify(res)}`;
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

  server.registerTool(
    'serial_read_tail',
    {
      title: 'Read latest serial data',
      description:
        'Read the most recent bytes received from the device. Rendered as UTF-8 text, or as a hex dump when the data looks binary. Default 4096 bytes, max 1048576.',
      inputSchema: {
        bytes: z
          .number()
          .int()
          .min(1)
          .max(MAX_CHUNK_BYTES)
          .optional()
          .describe(`How many trailing bytes to read (default ${DEFAULT_TAIL_BYTES}).`),
      },
      annotations: { readOnlyHint: true, openWorldHint: true },
    },
    handler(async ({ bytes }) => {
      const want = bytes ?? DEFAULT_TAIL_BYTES;
      const res = await client.request('tail', { bytes: want });
      const buf = decodeBase64(res?.base64);
      return renderChunk({
        buf,
        offset: Number(res?.offset ?? 0),
        totalBytes: Number(res?.total_bytes ?? 0),
      });
    })
  );

  server.registerTool(
    'serial_read_range',
    {
      title: 'Read a serial byte range',
      description:
        'Read an explicit byte range of the received stream (offsets match those reported by serial_read_tail and serial_wait_for). Rendered as text, or as a hex dump when the data looks binary.',
      inputSchema: {
        offset: z.number().int().min(0).describe('Absolute start offset in the received stream.'),
        length: z
          .number()
          .int()
          .min(1)
          .max(MAX_CHUNK_BYTES)
          .describe(`How many bytes to read (max ${MAX_CHUNK_BYTES}).`),
      },
      annotations: { readOnlyHint: true, openWorldHint: true },
    },
    handler(async ({ offset, length }) => {
      const res = await client.request('read_range', { offset, length });
      const buf = decodeBase64(res?.base64);
      return renderChunk({
        buf,
        offset: Number(res?.offset ?? offset),
        extra: [`length_read=${res?.length_read ?? buf.length}`],
      });
    })
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
