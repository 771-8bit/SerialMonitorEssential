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
  LINE_ENDINGS,
  decodeBase64,
  hexDump,
  looksLikeText,
  parseHex,
  renderBytes,
  waitForPattern,
} from './server.mjs';

const MAX_CHUNK = 1048576;

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

async function run() {
  await testMainFlow();
  await testAuth();
  await testBridgeAbsent();

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
