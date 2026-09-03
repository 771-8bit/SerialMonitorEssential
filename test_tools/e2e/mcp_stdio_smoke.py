#!/usr/bin/env python3
"""MCP stdio アダプタ（`--mcp`）のスモークテスト。GUI・com0com 不要（CI 可）。

ビルド済みバイナリを `--mcp` 付きで起動し、実パイプ越しに JSON-RPC を話して
プロトコル面を検証する:

  initialize ハンドシェイク / notifications/initialized の受け流し / ping /
  tools/list（7 ツール）/ ブリッジ未起動時の友好的エラー / 未知ツール(-32602) /
  未知メソッド(-32601) / stdin EOF でのクリーン終了

「ブリッジ未起動」を決定的にするため、子プロセスには未使用ポートを
SME_BRIDGE_PORT で渡す（開発機でアプリが AI Bridge を ON にしていても
結果が変わらない）。

使い方:
  python test_tools/e2e/mcp_stdio_smoke.py            # target/{debug,release} を自動検出
  python test_tools/e2e/mcp_stdio_smoke.py --exe <path-to-binary>
"""

import argparse
import json
import os
import subprocess
import sys

# ほぼ確実に誰も聞いていないポート（既定 57320 とは別）
ISOLATED_BRIDGE_PORT = "57399"


def default_exe() -> str:
    root = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
    name = "serial-monitor-essential" + (".exe" if os.name == "nt" else "")
    for profile in ("debug", "release"):
        candidate = os.path.join(root, "src-tauri", "target", profile, name)
        if os.path.isfile(candidate):
            return candidate
    return ""


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", default=default_exe(), help="path to the app binary")
    args = parser.parse_args()

    if not args.exe or not os.path.isfile(args.exe):
        print(
            "FAIL binary not found. Build it first: cd src-tauri && cargo build\n"
            f"     (looked for: {args.exe or 'src-tauri/target/{debug,release}/'})"
        )
        return 1

    env = dict(os.environ)
    env["SME_BRIDGE_PORT"] = ISOLATED_BRIDGE_PORT

    proc = subprocess.Popen(
        [args.exe, "--mcp"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        env=env,
    )
    failures = []

    def send(msg):
        proc.stdin.write(json.dumps(msg) + "\n")
        proc.stdin.flush()

    def recv():
        line = proc.stdout.readline()
        if not line:
            raise RuntimeError("unexpected EOF from server")
        return json.loads(line)

    def check(name, cond, detail=""):
        print(("PASS" if cond else "FAIL"), name, detail)
        if not cond:
            failures.append(name)

    try:
        # 1. initialize ハンドシェイク
        send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "smoke", "version": "0"}}})
        res = recv()
        server_name = res.get("result", {}).get("serverInfo", {}).get("name")
        check("initialize", server_name == "serial-monitor-essential",
              f"protocolVersion={res.get('result', {}).get('protocolVersion')}")

        # 2. initialized 通知（応答なし）の直後に ping が通る
        send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        send({"jsonrpc": "2.0", "id": 2, "method": "ping"})
        res = recv()
        check("ping-after-notification", res.get("id") == 2 and res.get("result") == {})

        # 3. tools/list: 7 ツール、全てにスキーマと説明がある
        send({"jsonrpc": "2.0", "id": 3, "method": "tools/list"})
        res = recv()
        tools = res.get("result", {}).get("tools", [])
        names = [t.get("name") for t in tools]
        shapes_ok = all(
            t.get("inputSchema", {}).get("type") == "object" and t.get("description")
            for t in tools
        )
        check("tools/list", len(tools) == 7 and "serial_wait_for" in names and shapes_ok,
              f"tools={len(tools)}")

        # 4. ブリッジ未起動 -> isError の友好的メッセージ（日英併記）
        send({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
              "params": {"name": "serial_status", "arguments": {}}})
        res = recv()
        r = res.get("result", {})
        text = r.get("content", [{}])[0].get("text", "")
        check("bridge-off-friendly-error",
              r.get("isError") is True and "AI Bridge" in text and "Cannot reach" in text,
              f"len={len(text)}")

        # 5. 未知ツール -> プロトコルエラー -32602
        send({"jsonrpc": "2.0", "id": 5, "method": "tools/call",
              "params": {"name": "nope", "arguments": {}}})
        res = recv()
        check("unknown-tool", res.get("error", {}).get("code") == -32602)

        # 6. 未知メソッド -> -32601
        send({"jsonrpc": "2.0", "id": 6, "method": "resources/list"})
        res = recv()
        check("unknown-method", res.get("error", {}).get("code") == -32601)

        # 7. stdin EOF でクリーン終了
        proc.stdin.close()
        proc.wait(timeout=15)
        check("clean-exit", proc.returncode == 0, f"rc={proc.returncode}")
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
