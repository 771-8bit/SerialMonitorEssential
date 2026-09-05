#!/usr/bin/env python3
"""AI Bridge のライブ E2E: 内蔵 MCP アダプタ（exe --mcp）経由で実往復を検証する。

前提（run_bridge_e2e.ps1 が自動化する。手動でやる場合）:
  1. アプリが起動し、設定画面で AI Bridge が ON（127.0.0.1:57320）
  2. アプリが com0com ペアの片側（既定 COM16）へ接続済み
  3. もう片側（既定 COM15）で pong_bot.py が動作中

検証項目:
  - initialize / serial_status（接続中ポート名の一致）
  - serial_send "PING" -> serial_wait_for "PONG \\d+" が MATCH
  - serial_read_tail に応答が見える
  - serial_wait_for のブロック中に ping が即応答（<2s）
  - notifications/cancelled で待ちが即中断され、応答が抑止される

使い方:
  python test_tools/e2e/mcp_bridge_live.py --exe <binary> [--expect-port COM16]
"""

import argparse
import json
import os
import subprocess
import sys
import time


def default_exe() -> str:
    root = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
    name = "serial-monitor-essential" + (".exe" if os.name == "nt" else "")
    for profile in ("release", "debug"):
        candidate = os.path.join(root, "src-tauri", "target", profile, name)
        if os.path.isfile(candidate):
            return candidate
    return ""


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", default=default_exe(), help="path to the app binary")
    parser.add_argument("--expect-port", default="COM16",
                        help="port name the app should be connected to")
    args = parser.parse_args()

    if not args.exe or not os.path.isfile(args.exe):
        print("FAIL binary not found (--exe). Build: cd src-tauri && cargo build --release")
        return 1

    proc = subprocess.Popen(
        [args.exe, "--mcp"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
    )
    failures = []
    state = {"id": 0}

    def call(method, params=None):
        state["id"] += 1
        proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": state["id"],
                                     "method": method, "params": params or {}}) + "\n")
        proc.stdin.flush()
        line = proc.stdout.readline()
        if not line:
            raise RuntimeError("unexpected EOF")
        return json.loads(line)

    def tool(name, tool_args=None):
        res = call("tools/call", {"name": name, "arguments": tool_args or {}})
        r = res.get("result", {})
        return r.get("isError", False), r.get("content", [{}])[0].get("text", "")

    def check(name, cond, detail=""):
        print(("PASS" if cond else "FAIL"), name, detail)
        if not cond:
            failures.append(name)

    try:
        res = call("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                  "clientInfo": {"name": "live-e2e", "version": "0"}})
        check("initialize", "result" in res)

        err, text = tool("serial_status")
        check("status-connected",
              not err and "connected: yes" in text and args.expect_port in text,
              text.splitlines()[0] if text else "")

        err, text = tool("serial_send", {"text": "PING", "line_ending": "lf"})
        check("send-ping", not err and "bytes_written=5" in text, text)

        err, text = tool("serial_wait_for", {"pattern": "PONG \\d+", "timeout_ms": 8000})
        check("wait-for-pong", not err and text.startswith("MATCH") and "PONG 42" in text,
              text.splitlines()[0] if text else "")

        err, text = tool("serial_read_tail", {"bytes": 64})
        check("read-tail", not err and "PONG 42" in text)

        # serial_wait_for のブロック中でも ping が即応答すること
        state["id"] += 1
        wait_id = state["id"]
        proc.stdin.write(json.dumps({
            "jsonrpc": "2.0", "id": wait_id, "method": "tools/call",
            "params": {"name": "serial_wait_for",
                       "arguments": {"pattern": "NEVER_MATCHES_XYZ",
                                     "timeout_ms": 30000}}}) + "\n")
        state["id"] += 1
        ping_id = state["id"]
        proc.stdin.write(json.dumps(
            {"jsonrpc": "2.0", "id": ping_id, "method": "ping"}) + "\n")
        proc.stdin.flush()
        t0 = time.time()
        res = json.loads(proc.stdout.readline())
        latency = time.time() - t0
        check("ping-during-wait", res.get("id") == ping_id and latency < 2.0,
              f"latency={latency:.2f}s")

        # cancelled で待ちを中断: 応答は来ない（次の応答は後続 ping のもの）
        proc.stdin.write(json.dumps({
            "jsonrpc": "2.0", "method": "notifications/cancelled",
            "params": {"requestId": wait_id, "reason": "e2e"}}) + "\n")
        proc.stdin.flush()
        t0 = time.time()
        state["id"] += 1
        ping2 = state["id"]
        proc.stdin.write(json.dumps(
            {"jsonrpc": "2.0", "id": ping2, "method": "ping"}) + "\n")
        proc.stdin.flush()
        res = json.loads(proc.stdout.readline())
        check("no-response-for-cancelled", res.get("id") == ping2)
        err, _ = tool("serial_status")
        elapsed = time.time() - t0
        check("worker-free-after-cancel", not err and elapsed < 5.0,
              f"elapsed={elapsed:.2f}s")

        proc.stdin.close()
        proc.wait(timeout=15)
    finally:
        if proc.poll() is None:
            proc.kill()

    if failures:
        print(f"FAILURES: {failures}")
        return 1
    print("ALL PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
