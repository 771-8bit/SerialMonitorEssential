#!/usr/bin/env python3
"""
FT232H 自己ループバック（D0=TX -> D1=RX をジャンパ）のトランスポート基準測定。

アプリを介さず pyserial だけで同じポートに書いて読み、
  - 欠落 0・破損 0（送ったバイト列と受けたバイト列の SHA-256 一致）
  - 実効スループットが設定ボーレートに追随する（= ボーレートが本当に効いている）
を確かめる。アプリ側試験（app_loopback_12m.py）の前提となるオラクル。

使い方:
    python loopback_baseline.py --port COM5 --baud 12000000 --seconds 60
    python loopback_baseline.py --port COM5 --sweep          # 複数ボーレートで短時間ずつ
    python loopback_baseline.py --port COM5 --rtscts         # D2(RTS)-D3(CTS) も結んだ場合
"""

import argparse
import hashlib
import sys
import threading
import time

import serial

CHUNK = 64 * 1024


def pattern_block(index: int) -> bytes:
    """決定的なパターン: 64KiB ごとに先頭へ通し番号を埋め、残りは 0..255 の繰り返し。"""
    body = bytearray((i + index) & 0xFF for i in range(CHUNK))
    body[0:8] = index.to_bytes(8, "little")
    return bytes(body)


def run(port: str, baud: int, seconds: float, rtscts: bool = False) -> dict:
    # 64KiB の書き込みは 115200 では 5.7 秒かかる。線速度に比例した余裕を取る。
    write_timeout = CHUNK / (baud / 10) * 3 + 2
    ser = serial.Serial(port, baud, timeout=0.05, write_timeout=write_timeout, rtscts=rtscts)
    ser.reset_input_buffer()
    ser.reset_output_buffer()

    tx_hash = hashlib.sha256()
    rx_hash = hashlib.sha256()
    tx_bytes = 0
    rx_bytes = 0
    tx_done = threading.Event()
    tx_error = []

    def writer():
        nonlocal tx_bytes
        index = 0
        deadline = time.perf_counter() + seconds
        try:
            while time.perf_counter() < deadline:
                block = pattern_block(index)
                written = ser.write(block)
                # 部分書き込み（タイムアウト）でも実際に出た分だけをハッシュする
                tx_hash.update(block[:written])
                tx_bytes += written
                if written != len(block):
                    raise RuntimeError(f"short write {written}/{len(block)}")
                index += 1
            ser.flush()
        except Exception as e:  # noqa: BLE001 - 測定を止めて報告する
            tx_error.append(repr(e))
        finally:
            tx_done.set()

    started = time.perf_counter()
    thread = threading.Thread(target=writer, daemon=True)
    thread.start()

    last_rx = time.perf_counter()
    while True:
        data = ser.read(CHUNK)
        if data:
            rx_hash.update(data)
            rx_bytes += len(data)
            last_rx = time.perf_counter()
        elif tx_done.is_set() and (rx_bytes >= tx_bytes or time.perf_counter() - last_rx > 2.0):
            break
    # 末尾の無受信待ち（最大 2 秒）はスループットに含めない
    elapsed = last_rx - started
    thread.join()
    ser.close()

    return {
        "baud": baud,
        "tx": tx_bytes,
        "rx": rx_bytes,
        "elapsed": elapsed,
        "rate": rx_bytes / elapsed,
        "theoretical": baud / 10,  # 8N1 = 10 bit/byte
        "match": not tx_error and rx_bytes == tx_bytes and tx_hash.digest() == rx_hash.digest(),
        "error": tx_error[0] if tx_error else "",
    }


def report(r: dict) -> None:
    eff = r["rate"] / r["theoretical"] * 100
    verdict = "PASS" if r["match"] else "FAIL"
    print(
        f"{verdict}  baud={r['baud']:>9,}  tx={r['tx']:>12,}  rx={r['rx']:>12,}  "
        f"lost={r['tx'] - r['rx']:>8,}  {r['rate'] / 1e6:6.3f} MB/s "
        f"({eff:5.1f}% of {r['theoretical'] / 1e6:.3f} MB/s)  t={r['elapsed']:.1f}s"
        + (f"  error={r['error']}" if r["error"] else "")
    )


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--port", default="COM5")
    p.add_argument("--baud", type=int, default=12_000_000)
    p.add_argument("--seconds", type=float, default=60)
    p.add_argument("--sweep", action="store_true", help="115200〜12M を数秒ずつ測る")
    p.add_argument("--rtscts", action="store_true", help="ハードウェアフロー制御（D2=RTS -> D3=CTS のジャンパが必要）")
    args = p.parse_args()

    if args.sweep:
        results = [run(args.port, b, s, args.rtscts) for b, s in [
            (115_200, 3), (921_600, 3), (3_000_000, 3), (6_000_000, 3), (12_000_000, 5),
        ]]
    else:
        results = [run(args.port, args.baud, args.seconds, args.rtscts)]
    for r in results:
        report(r)
    return 0 if all(r["match"] for r in results) else 1


if __name__ == "__main__":
    sys.exit(main())
