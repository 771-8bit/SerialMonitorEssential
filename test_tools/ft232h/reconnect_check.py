#!/usr/bin/env python3
"""
自動再接続の実機確認（SYS-F-807 / docs/27 OV-1）。人が USB を抜き差しする。

前提: アプリが FT232H（D0–D1 ループバック）のポートに接続済みで、
      MCP Server と Auto Connect が ON（127.0.0.1:57320）。

やること:
  1. 200 ms ごとにブリッジの `send` で "tick N" を送る（ループバックで自分が受信する）
  2. 実行中に人が USB ケーブルを抜き、数秒後に挿し直す
  3. 次を判定する:
     - 切断 → 再接続が起きた
     - 待機中に `status.reconnecting` が true だった
     - `total_bytes` が一度も巻き戻らなかった（同じキャプチャに追記が続いた）
     - 抜く前の行（"tick 0" から）が残っており、行の順序が保たれている
     （判定は開始時点の total_bytes 以降のデータだけで行う）

使い方:
    python reconnect_check.py --seconds 300
"""

from __future__ import annotations

import argparse
import base64
import json
import socket
import sys
import time


def rpc(host: str, port: int, method: str, params: dict | None = None) -> dict:
    with socket.create_connection((host, port), timeout=5) as conn:
        request = {"id": 1, "method": method, "params": params or {}}
        conn.sendall((json.dumps(request) + "\n").encode())
        return json.loads(conn.makefile().readline())


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[1])
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--bridge-port", type=int, default=57320)
    parser.add_argument("--seconds", type=float, default=300, help="give up after this long")
    args = parser.parse_args()

    def call(method: str, params: dict | None = None) -> dict:
        return rpc(args.host, args.bridge_port, method, params)

    deadline = time.time() + args.seconds
    sent = 0
    phase = "before"  # before -> lost -> back
    # Judge only what this run sent (the capture may already hold earlier data)
    start_offset = last_total = call("status")["result"]["total_bytes"]
    saw_reconnecting = False
    t_lost = t_back = 0.0
    fails: list[str] = []

    print("Unplug the USB cable, wait a few seconds, then plug it back in.", flush=True)
    while time.time() < deadline:
        status = call("status")["result"]
        total = status["total_bytes"]
        if total < last_total:
            fails.append(f"total_bytes went backwards {last_total} -> {total}")
        last_total = total

        if status["connected"]:
            if call("send", {"text": f"tick {sent}", "line_ending": "lf"}).get("ok"):
                sent += 1
            if phase == "lost":
                phase, t_back = "back", time.time()
                print(f"reconnected at total={total} after {t_back - t_lost:.1f}s", flush=True)
            elif phase == "back" and time.time() - t_back > 3:
                break
        else:
            if phase == "before":
                phase, t_lost = "lost", time.time()
                print(f"lost at total={total}", flush=True)
            saw_reconnecting = saw_reconnecting or bool(status.get("reconnecting"))
        time.sleep(0.2)

    raw = call("read_range", {"offset": start_offset, "length": 1 << 20})["result"]["base64"]
    lines = [line for line in base64.b64decode(raw).decode(errors="replace").split("\n") if line]
    print(f"sent={sent} captured_lines={len(lines)} first={lines[:1]} last={lines[-1:]}")

    if phase != "back":
        fails.append(f"no unplug/replug was observed (phase={phase})")
    if phase != "before" and not saw_reconnecting:
        fails.append("status.reconnecting was never true while disconnected")
    if not lines or lines[0] != "tick 0":
        fails.append("the capture from before the unplug was not kept (first line is not 'tick 0')")
    numbers = [int(line.split()[1]) for line in lines if line.startswith("tick ") and line[5:].isdigit()]
    if numbers != sorted(numbers):
        fails.append("lines are out of order")

    if fails:
        print("FAIL\n" + "\n".join(fails))
        return 1
    print("PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
