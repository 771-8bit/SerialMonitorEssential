#!/usr/bin/env node
/**
 * Standalone smoke test for the MCP server's bridge client.
 *
 * No MCP involved: this file fakes the BRIDGE side (the TCP server that the
 * Tauri app normally provides) on an ephemeral port, then drives the exported
 * bridge-client functions of server.mjs against it.
 *
 *   node smoke.mjs      -> exits 0 and prints PASS lines when everything works
 */

import net from 'node:net';

import {
  BridgeClient,
  DEFAULT_MAX_CHARS,
  LINE_ENDINGS,
  MIN_MAX_CHARS,
  collapseRepeatedLines,
  decodeBase64,
  headBytesWithinCharBudget,
  hexDump,
  insightText,
  looksLikeText,
  parseHex,
  renderBytes,
  renderChunk,
  renderFind,
  tailBytesWithinCharBudget,
  waitForPattern,
} from './server.mjs';

const MAX_CHUNK = 1048576;

/** First line of a multi-line report (used as the detail in check messages). */
const firstLine = (text) => String(text).split(/\r?\n/)[0];

/* ------------------------------------------------------------------ */
/* Fake bridge (the app side of the protocol)                          */
/* ------------------------------------------------------------------ */

async function createFakeBridge({ token = null, initial = Buffer.alloc(0) } = {}) {
  const state = {
    data: Buffer.from(initial),
    sent: [],
  };

  const handle = (req, session) => {
    const params = req.params ?? {};
    switch (req.method) {
      case 'auth':
        if (!token || params.token !== token) throw new Error('invalid token');
        session.authed = true;
        return {};
      case 'status':
        return {
          connected: true,
          port_name: 'COM7',
          total_bytes: state.data.length,
          app_version: '0.0.0-fake',
          protocol: 1,
        };
      case 'tail': {
        const want = Math.min(Number(params.bytes ?? 4096), MAX_CHUNK);
        if (!(want > 0)) throw new Error('bytes must be positive');
        const start = Math.max(0, state.data.length - want);
        const slice = state.data.subarray(start);
        return {
          base64: slice.toString('base64'),
          offset: start,
          total_bytes: state.data.length,
        };
      }
      // The real scan lives in the Rust bridge (insight/find.rs); the fake only
      // has to answer in the same shape so the client wiring can be exercised.
      case 'find': {
        const re = new RegExp(String(params.pattern ?? ''), 'gm');
        const text = state.data.toString('binary');
        const hits = [];
        let matches = 0;
        let first = null;
        let last = null;
        for (const m of text.matchAll(re)) {
          matches += 1;
          if (first === null) first = m.index;
          last = m.index;
          if (hits.length < Number(params.max_hits ?? 20)) {
            const lineStart = text.lastIndexOf('\n', m.index) + 1;
            const nl = text.indexOf('\n', m.index);
            hits.push({
              offset: m.index,
              len: m[0].length,
              excerpt: text.slice(lineStart, nl < 0 ? text.length : nl),
              excerpt_truncated: false,
            });
          }
        }
        return {
          matches,
          exact: true,
          complete: true,
          hits,
          first,
          last,
          range_start: 0,
          range_end: state.data.length,
          bytes_scanned: state.data.length,
          gap_bytes: 0,
          total_bytes: state.data.length,
          elapsed_ms: 1,
        };
      }
      case 'read_range': {
        const offset = Number(params.offset ?? 0);
        const length = Math.min(Number(params.length ?? 0), MAX_CHUNK);
        if (offset < 0 || !(length > 0)) throw new Error('bad range');
        const slice = state.data.subarray(offset, offset + length);
        return {
          base64: slice.toString('base64'),
          offset,
          length_read: slice.length,
        };
      }
      case 'send': {
        let buf;
        if (typeof params.base64 === 'string') {
          buf = Buffer.from(params.base64, 'base64');
        } else {
          const ending = LINE_ENDINGS[params.line_ending ?? 'none'];
          if (ending === undefined) throw new Error(`bad line_ending: ${params.line_ending}`);
          buf = Buffer.from(`${String(params.text ?? '')}${ending}`, 'utf8');
        }
        state.sent.push(buf);
        return { bytes_written: buf.length };
      }
      // The real summarising lives in the Rust bridge (insight/*). The fake
      // only has to answer in the same envelope shape, so the adapter's
      // "thin renderer" contract (docs/26 §2.3) can be exercised.
      case 'digest':
      case 'watch':
        return {
          text: `${req.method} gen=1
ALARM 0 err · 0 warn (exact, coverage 100%)
cursor v1:g1:o${state.data.length}:l1:e1
- nothing withheld`,
          data: { cursor: `v1:g1:o${state.data.length}:l1:e1`, echo: params },
          meta: { gen: 1, exact: true, coverage_pct: 100 },
          budget: { max_chars: 1800, used_chars: 100, truncated: false },
        };
      case 'ports':
        return ['COM3', 'COM7'];
      default:
        throw new Error(`unknown method: ${req.method}`);
    }
  };

  const server = net.createServer((socket) => {
    const session = { authed: !token };
    let rx = '';
    socket.setEncoding('utf8');
    socket.on('error', () => {});
    socket.on('data', (chunk) => {
      rx += chunk;
      let index;
      while ((index = rx.indexOf('\n')) >= 0) {
        const line = rx.slice(0, index).trim();
        rx = rx.slice(index + 1);
        if (!line) continue;
        let req;
        try {
          req = JSON.parse(line);
        } catch {
          continue;
        }
        let response;
        try {
          if (!session.authed && req.method !== 'auth') throw new Error('unauthorized');
          response = { id: req.id, ok: true, result: handle(req, session) };
        } catch (err) {
          response = { id: req.id, ok: false, error: err.message };
        }
        socket.write(`${JSON.stringify(response)}\n`);
      }
    });
  });

  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));

  return {
    port: server.address().port,
    state,
    append(data) {
      state.data = Buffer.concat([state.data, Buffer.from(data)]);
    },
    close() {
      return new Promise((resolve) => server.close(resolve));
    },
  };
}

/** An ephemeral port that nothing is listening on. */
async function findClosedPort() {
  const probe = net.createServer();
  await new Promise((resolve) => probe.listen(0, '127.0.0.1', resolve));
  const { port } = probe.address();
  await new Promise((resolve) => probe.close(resolve));
  return port;
}

/* ------------------------------------------------------------------ */
/* Tiny assertion harness                                              */
/* ------------------------------------------------------------------ */

let failures = 0;

function check(name, condition, detail = '') {
  if (condition) {
    console.log(`PASS ${name}${detail ? ` - ${detail}` : ''}`);
  } else {
    failures++;
    console.error(`FAIL ${name}${detail ? ` - ${detail}` : ''}`);
  }
}

/* ------------------------------------------------------------------ */
/* Tests                                                               */
/* ------------------------------------------------------------------ */

const CANNED = 'boot ok\r\nversion 1.2.3\r\n> ';

async function testMainFlow() {
  const bridge = await createFakeBridge({ initial: Buffer.from(CANNED, 'utf8') });
  const client = new BridgeClient({ host: '127.0.0.1', port: bridge.port, timeoutMs: 2000 });

  try {
    // 1. status roundtrip
    const status = await client.request('status', {});
    check(
      'status roundtrip',
      status.connected === true &&
        status.port_name === 'COM7' &&
        status.total_bytes === CANNED.length &&
        status.protocol === 1,
      `port=${status.port_name} total_bytes=${status.total_bytes}`
    );

    // ports (bonus): result is a bare array per the spec
    const ports = await client.request('ports', {});
    check(
      'ports returns string array',
      Array.isArray(ports) && ports.includes('COM7'),
      JSON.stringify(ports)
    );

    // 2. tail base64 decode
    const tail = await client.request('tail', { bytes: 8 });
    const tailBuf = decodeBase64(tail.base64);
    check(
      'tail base64 decode',
      tailBuf.length === 8 &&
        tailBuf.toString('utf8') === CANNED.slice(-8) &&
        tail.offset === CANNED.length - 8 &&
        tail.total_bytes === CANNED.length,
      `offset=${tail.offset} text=${JSON.stringify(tailBuf.toString('utf8'))}`
    );

    // 3. send with line ending appended
    const sendRes = await client.request('send', { text: 'AT+VER', line_ending: 'crlf' });
    const received = bridge.state.sent[bridge.state.sent.length - 1];
    check(
      'send with line ending appended',
      sendRes.bytes_written === 8 &&
        received.toString('utf8') === 'AT+VER\r\n' &&
        received.length === 8,
      `bytes_written=${sendRes.bytes_written} payload=${JSON.stringify(received.toString('utf8'))}`
    );

    // send_hex path (bonus): hex pairs -> base64 -> raw bytes, nothing appended
    const hexBuf = parseHex('01 03 00:00 0x0A');
    const hexRes = await client.request('send', { base64: hexBuf.toString('base64') });
    const hexReceived = bridge.state.sent[bridge.state.sent.length - 1];
    check(
      'send_hex validates pairs and sends raw bytes',
      hexRes.bytes_written === 5 && hexReceived.toString('hex') === '010300000a',
      `hex=${hexReceived.toString('hex')}`
    );

    let hexRejected = false;
    try {
      parseHex('0102 3');
    } catch {
      hexRejected = true;
    }
    check('send_hex rejects odd digit count', hexRejected);

    // read_range + binary rendering (bonus)
    bridge.append(Buffer.from([0x00, 0xff, 0x01, 0x80, 0x02, 0x7f, 0x03, 0xfe]));
    const rangeStart = CANNED.length;
    const range = await client.request('read_range', { offset: rangeStart, length: 8 });
    const rangeBuf = decodeBase64(range.base64);
    const rendered = renderBytes(rangeBuf, range.offset);
    check(
      'read_range hex dump for binary data',
      range.length_read === 8 &&
        !looksLikeText(rangeBuf) &&
        rendered.mode === 'hex' &&
        rendered.body === hexDump(rangeBuf, rangeStart) &&
        rendered.body.includes('00 ff 01 80 02 7f 03 fe'),
      rendered.body.split('\n')[0]
    );

    // find round-trip: counts and offsets come back, bulk data does not
    const found = await client.request('find', { pattern: '\\d\\.\\d', max_hits: 5 });
    const foundText = renderFind(found);
    check(
      'find returns counts and handles, not payload',
      found.matches > 0 &&
        found.exact === true &&
        !JSON.stringify(found).includes('base64') &&
        foundText.startsWith(`matches: ${found.matches} (exact)`) &&
        foundText.includes(`first @off=${found.first}`),
      foundText.split('\n')[0]
    );

    // digest / watch: the adapter is a pass-through, and it forwards args
    const digest = await insightText(client, 'digest', {
      window: '30s',
      top: 12,
      max_chars: 2000,
      cursor: undefined,
    });
    check(
      'digest passes the bridge text through untouched',
      digest.startsWith('digest gen=1') &&
        digest.includes('ALARM 0 err') &&
        digest.includes('coverage 100%') &&
        digest.includes('cursor v1:g1:') &&
        !digest.includes('base64'),
      firstLine(digest)
    );
    const echoed = (await client.request('digest', { window: '30s', top: 12 })).data.echo;
    check(
      'digest forwards only the arguments it was given',
      echoed.window === '30s' && echoed.top === 12 && !('cursor' in echoed),
      JSON.stringify(echoed)
    );
    const watched = await insightText(client, 'watch', { cursor: 'v1:g1:o1:l1:e1', wait_ms: 100 });
    check('watch passes the bridge text through', watched.startsWith('watch gen=1'), firstLine(watched));

    // A reply with no text must fail loudly rather than read as "nothing happened"
    let textless = null;
    try {
      await insightText({ request: async () => ({ data: {} }) }, 'digest', {});
    } catch (err) {
      textless = String(err?.message ?? err);
    }
    check('a reply without text is an error, not an empty answer', textless?.includes('no text'), String(textless));

    // 4. wait_for-style polling match
    const before = bridge.state.data.length;
    setTimeout(() => bridge.append('PONG 42\r\n'), 700);
    const waited = await waitForPattern(client, {
      pattern: 'PONG (\\d+)',
      timeoutMs: 5000,
      fromEnd: true,
    });
    check(
      'wait_for polling match',
      waited.matched === true &&
        waited.match === 'PONG 42' &&
        waited.offset === before &&
        waited.excerpt.includes('PONG 42') &&
        waited.elapsedMs < 5000,
      `offset=${waited.offset} elapsed_ms=${waited.elapsedMs} match=${JSON.stringify(waited.match)}`
    );

    // wait_for timeout report (bonus)
    const timedOut = await waitForPattern(client, {
      pattern: 'NEVER_APPEARS',
      timeoutMs: 700,
      fromEnd: false,
      pollMs: 200,
    });
    check(
      'wait_for timeout reports last bytes seen',
      timedOut.matched === false &&
        timedOut.tail.length > 0 &&
        timedOut.tail.length <= 256 &&
        timedOut.elapsedMs >= 700,
      `bytes_scanned=${timedOut.bytesScanned} elapsed_ms=${timedOut.elapsedMs}`
    );
  } finally {
    client.close();
    await bridge.close();
  }
}

async function testAuth() {
  const bridge = await createFakeBridge({ token: 's3cret', initial: Buffer.from('ready\n') });
  const good = new BridgeClient({
    host: '127.0.0.1',
    port: bridge.port,
    token: 's3cret',
    timeoutMs: 2000,
  });
  const bad = new BridgeClient({
    host: '127.0.0.1',
    port: bridge.port,
    token: 'wrong',
    timeoutMs: 2000,
  });
  try {
    const status = await good.request('status', {});
    check('token auth handshake', status.connected === true, `port=${status.port_name}`);

    let rejected = null;
    try {
      await bad.request('status', {});
    } catch (err) {
      rejected = err;
    }
    check(
      'wrong token is rejected with a bridge error',
      rejected !== null && /invalid token/.test(rejected.message),
      rejected ? rejected.message.split('\n')[0] : 'no error thrown'
    );
  } finally {
    good.close();
    bad.close();
    await bridge.close();
  }
}

async function testBridgeAbsent() {
  const port = await findClosedPort();
  const client = new BridgeClient({ host: '127.0.0.1', port, timeoutMs: 2000 });
  const started = Date.now();
  let error = null;
  try {
    await client.request('status', {});
  } catch (err) {
    error = err;
  }
  const elapsed = Date.now() - started;
  client.close();
  check(
    'bridge absent fails fast with a friendly message',
    error !== null &&
      error.name === 'BridgeError' &&
      elapsed < 2000 &&
      error.message.includes('AI Bridge') &&
      error.message.includes('SerialMonitorEssential') &&
      /接続できません/.test(error.message),
    `elapsed_ms=${elapsed} first_line=${error ? error.message.split('\n')[0] : 'no error'}`
  );
}

/**
 * Rendering budget (docs/26 §7 phase 0). No bridge needed: renderChunk is pure.
 * The Rust adapter is the shipped implementation and carries the full test set;
 * these keep the reference implementation honest about the same contract.
 */
function testRenderBudget() {
  const ascii = Buffer.alloc(40000, 0x41);
  const binary = Buffer.from(Array.from({ length: 40000 }, (_, i) => i % 251));
  const jp = Buffer.from('温度 25.5 度\n'.repeat(3000), 'utf8');

  let worst = 0;
  let over = null;
  for (const buf of [ascii, binary, jp]) {
    for (const maxChars of [MIN_MAX_CHARS, 1000, DEFAULT_MAX_CHARS, 20000]) {
      for (const keepTail of [true, false]) {
        for (const format of ['auto', 'text', 'hex', 'lines']) {
          for (const collapse of [false, true]) {
            const out = renderChunk({
              buf,
              offset: 1000,
              format,
              collapse,
              maxChars,
              keepTail,
            });
            const n = [...out.text].length;
            worst = Math.max(worst, n - maxChars);
            if (n > maxChars) over = `${format}/${keepTail}/${maxChars}: ${n} chars`;
          }
        }
      }
    }
  }
  check('render budget is never exceeded', over === null, over ?? `worst margin ${worst}`);

  // The pathological case from docs/26: 4096 binary bytes rendered as hex.
  const unbounded = hexDump(binary.subarray(0, 4096), 0).length;
  const bounded = renderChunk({ buf: binary.subarray(0, 4096), offset: 0 });
  check(
    'hex tail is bounded and discloses truncation',
    unbounded > 19000 &&
      [...bounded.text].length <= DEFAULT_MAX_CHARS &&
      bounded.text.includes('truncated:'),
    `${unbounded} chars unbounded -> ${[...bounded.text].length} chars`
  );

  // The truncation footer must be a usable handle, not just a warning.
  const range = renderChunk({
    buf: Buffer.alloc(10000, 0x78),
    offset: 5000,
    format: 'text',
    maxChars: 1000,
    keepTail: false,
  });
  const dropped = 10000 - (range.shownEnd - range.shownStart);
  check(
    'truncation footer resumes at an exact offset',
    range.text.includes(`serial_read_range(offset=${range.shownEnd}, length=${dropped})`),
    range.text.split('\n').pop()
  );

  // find rendering: a zero must never be bare (docs/26 §6.2 rule 5).
  const base = {
    hits: [],
    first: null,
    last: null,
    range_start: 0,
    range_end: 52400000,
    bytes_scanned: 52400000,
    gap_bytes: 0,
    elapsed_ms: 812,
  };
  const cleanZero = renderFind({ ...base, matches: 0, exact: true, complete: true });
  check(
    'find reports an exact zero with its coverage',
    cleanZero.startsWith('matches: 0 (exact)') && cleanZero.includes('(100%)'),
    cleanZero.split('\n')[0]
  );
  const cutShort = renderFind({
    ...base,
    matches: 0,
    exact: false,
    complete: false,
    bytes_scanned: 8000000,
  });
  check(
    'find never passes off a partial scan as a definitive zero',
    cutShort.includes('AT LEAST') &&
      cutShort.includes('scan cut short') &&
      cutShort.includes('resume: serial_find(pattern=..., from=8000000'),
    cutShort.split('\n')[0]
  );

  const { text: collapsed, removed } = collapseRepeatedLines('a\na\na\nb\nc\nc');
  check(
    'repeated lines collapse',
    collapsed === 'a  [x3]\nb\nc  [x2]' && removed === 3,
    `${JSON.stringify(collapsed)} removed=${removed}`
  );

  // CJK must be budgeted by characters, not bytes (3 bytes per char).
  check(
    'char budget counts characters, not bytes',
    headBytesWithinCharBudget(Buffer.from('温度センサ', 'utf8'), 2) === 6 &&
      tailBytesWithinCharBudget(Buffer.from('温度センサ', 'utf8'), 2) === 6,
    'CJK 2 chars = 6 bytes at both ends'
  );
}

async function run() {
  await testMainFlow();
  await testAuth();
  await testBridgeAbsent();
  testRenderBudget();

  if (failures > 0) {
    console.error(`\n${failures} check(s) FAILED`);
    process.exit(1);
  }
  console.log('\nALL PASS');
  process.exit(0);
}

run().catch((err) => {
  console.error('FAIL smoke test crashed -', err?.stack ?? err);
  process.exit(1);
});
