#!/usr/bin/env python3
"""
アプリ経由の FT232H 自己ループバック試験（SYS-NF-101 / DEBT-9）。

前提: アプリが --port を --baud で開いており、MCP Server が ON（127.0.0.1:57320）。
      FT232H の D0(TX) と D1(RX) をジャンパで結んでいる。
      （run_app_loopback.ps1 が起動・UIA 設定・後片付けまで一括で行う）

やること:
  1. ブリッジの `send` で決定的なテキスト行を連続送信する（アプリが TX し、自分で RX する）
  2. 並行して別接続で `status` / `digest` を 1 秒ごとに取り、受信量と索引遅れ
     （meta.index_lag_bytes）を記録する
  3. 送信後、`read_range` でキャプチャを読み戻し、送ったバイト列と SHA-256 を比較する
     （欠落があれば最初の不一致位置と欠落量を報告）
  4. 索引遅れが 0 に戻るまでの時間を測る

注意（解釈）: 送信元がアプリ自身なので「外部デバイスからの受信」としては純粋ではない。
また現行実装は送信中にポートの Mutex を保持し、その間受信ワーカーが読めない
（port.rs::write / worker_thread.rs）。--send-size を大きくすると受信停止時間が伸びる。

使い方:
    python app_loopback.py --port COM5 --baud 12000000 --seconds 60 --send-size 4096
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import socket
import sys
import threading
import time

READ_MAX = 1 << 20


class Bridge:
    def __init__(self, host: str, port: int, token: str | None):
        self.sock = socket.create_connection((host, port), timeout=30)
        self.file = self.sock.makefile("rb")
        self.next_id = 1
        if token:
            self.call("auth", {"token": token})

    def call(self, method: str, params: dict | None = None) -> dict:
        req = {"id": self.next_id, "method": method, "params": params or {}}
        self.next_id += 1
        self.sock.sendall((json.dumps(req) + "\n").encode())
        line = self.file.readline()
        if not line:
            raise RuntimeError("bridge closed the connection")
        resp = json.loads(line)
        if not resp.get("ok"):
            raise RuntimeError(f"{method}: {resp.get('error')}")
        return resp["result"]

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass


def line_stream():
    """決定的なセンサーログ風テキスト（索引が意味を持つ行指向データ）。"""
    seq = 0
    while True:
        yield (
            f"seq={seq:010d} t_us={seq * 997:012d} ch0={(seq * 7) % 1000:4d} "
            f"ch1={(seq * 13) % 4096:5d} state={'OK' if seq % 97 else 'WARN'}\n"
        ).encode()
        seq += 1


def make_chunks(size: int):
    buf = bytearray()
    for line in line_stream():
        buf += line
        while len(buf) >= size:
            yield bytes(buf[:size])
            del buf[:size]


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--port", default="COM5", help="アプリが開いているはずのポート（検査用）")
    p.add_argument("--baud", type=int, default=12_000_000, help="理論スループット計算用")
    p.add_argument("--seconds", type=float, default=60)
    p.add_argument("--send-size", type=int, default=4096, help="1 回の send のバイト数")
    p.add_argument("--bridge-host", default="127.0.0.1")
    p.add_argument("--bridge-port", type=int, default=57320)
    p.add_argument("--token", default=None)
    p.add_argument("--settle", type=float, default=30, help="送信後に受信/索引の追随を待つ上限秒")
    args = p.parse_args()

    ctl = Bridge(args.bridge_host, args.bridge_port, args.token)
    st = ctl.call("status")
    if not st.get("connected"):
        print("FAIL: app is not connected to a port")
        return 2
    if args.port and st.get("port_name") != args.port:
        print(f"FAIL: app holds {st.get('port_name')!r}, expected {args.port!r}")
        return 2
    start = st["total_bytes"]
    print(f"app {st.get('app_version')} on {st['port_name']}, capture starts at offset {start:,}")

    # --- 監視スレッド（別接続） ---
    samples = []  # (t, total_bytes, index_lag_bytes)
    stop_mon = threading.Event()

    def monitor():
        mon = Bridge(args.bridge_host, args.bridge_port, args.token)
        t0 = time.perf_counter()
        while not stop_mon.is_set():
            total = mon.call("status")["total_bytes"]
            lag = None
            try:
                lag = mon.call("digest", {"max_chars": 200, "buckets": 0})["meta"]["index_lag_bytes"]
            except RuntimeError as e:  # 索引が無い構成でも受信検証は続ける
                lag = f"err:{e}"
            samples.append((time.perf_counter() - t0, total - start, lag))
            stop_mon.wait(1.0)
        mon.close()

    mon_thread = threading.Thread(target=monitor, daemon=True)
    mon_thread.start()

    # --- 送信 ---
    tx_hash = hashlib.sha256()
    sent = 0
    sends = 0
    max_send_ms = 0.0
    t_start = time.perf_counter()
    deadline = t_start + args.seconds
    for chunk in make_chunks(args.send_size):
        if time.perf_counter() >= deadline:
            break
        t = time.perf_counter()
        try:
            res = ctl.call("send", {"base64": base64.b64encode(chunk).decode()})
        except RuntimeError as e:
            # 送信エラー（例: フロー制御下の書き込みタイムアウト）。一部は線に出ている可能性が
            # あり以降の比較は無意味なので、ここで打ち切って報告する。
            stop_mon.set()
            print(f"send #{sends + 1} failed after {(time.perf_counter() - t) * 1000:.0f} ms "
                  f"({sent:,} B sent OK before it): {e}")
            print("RESULT: FAIL (send error)")
            return 1
        max_send_ms = max(max_send_ms, (time.perf_counter() - t) * 1000)
        if res["bytes_written"] != len(chunk):
            print(f"FAIL: short write {res['bytes_written']}/{len(chunk)}")
            return 1
        tx_hash.update(chunk)
        sent += len(chunk)
        sends += 1
    t_sent = time.perf_counter() - t_start
    print(f"sent {sent:,} B in {sends:,} sends over {t_sent:.1f}s = {sent / t_sent / 1e6:.3f} MB/s "
          f"({sent / t_sent / (args.baud / 10) * 100:.1f}% of line rate), slowest send {max_send_ms:.0f} ms")

    # --- 受信の追随待ち ---
    t_wait = time.perf_counter()
    last_total, last_change = -1, time.perf_counter()
    while time.perf_counter() - t_wait < args.settle:
        got = ctl.call("status")["total_bytes"] - start
        if got >= sent:
            break
        if got != last_total:
            last_total, last_change = got, time.perf_counter()
        elif time.perf_counter() - last_change > 3:
            break
        time.sleep(0.2)
    received = ctl.call("status")["total_bytes"] - start

    # --- 索引の追随待ち ---
    lag_zero_after = None
    t_idx = time.perf_counter()
    while time.perf_counter() - t_idx < args.settle:
        try:
            lag = ctl.call("digest", {"max_chars": 200, "buckets": 0})["meta"]["index_lag_bytes"]
        except RuntimeError:
            break
        if lag == 0:
            lag_zero_after = time.perf_counter() - t_idx
            break
        time.sleep(0.2)
    stop_mon.set()
    mon_thread.join(timeout=5)

    # --- 読み戻し検証 ---
    rx_hash = hashlib.sha256()
    expected = make_chunks(args.send_size)
    exp_buf = bytearray()
    first_diff = None
    offset = start
    while offset < start + received:
        r = ctl.call("read_range", {"offset": offset, "length": READ_MAX})
        data = base64.b64decode(r["base64"])
        if not data:
            break
        rx_hash.update(data)
        if first_diff is None:
            while len(exp_buf) < len(data):
                exp_buf += next(expected)
            if exp_buf[: len(data)] != data:
                k = next(i for i in range(len(data)) if exp_buf[i] != data[i])
                first_diff = offset - start + k
            del exp_buf[: len(data)]
        offset += len(data)
    ctl.close()

    lags = [s[2] for s in samples if isinstance(s[2], int)]
    print(f"received {received:,} B, lost {sent - received:,} B")
    if lags:
        print(f"index_lag_bytes during run: max {max(lags):,}  last {lags[-1]:,}  "
              f"(samples {len(lags)})")
    print("index caught up (lag 0) "
          + (f"{lag_zero_after:.1f}s after send end" if lag_zero_after is not None else "NOT within settle time"))
    ok = received == sent and tx_hash.digest() == rx_hash.digest()
    if first_diff is not None:
        print(f"first mismatch at capture byte {first_diff:,}")
    print(f"SHA-256 sent {tx_hash.hexdigest()[:16]}…  captured {rx_hash.hexdigest()[:16]}…")
    print("RESULT: " + ("PASS (zero loss, zero corruption)" if ok else "FAIL"))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
