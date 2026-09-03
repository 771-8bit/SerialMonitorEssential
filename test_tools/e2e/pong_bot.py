#!/usr/bin/env python3
"""AI Bridge E2E 用の応答ボット: "PING" 行に "PONG 42" を返す。

com0com ペアの片側（既定 COM15）で待ち、アプリ（もう片側 COM16 を開く）
経由で AI が送った PING に応答する。Ctrl+C で終了。

使い方:
  python test_tools/e2e/pong_bot.py             # COM15 @115200
  python test_tools/e2e/pong_bot.py --port COM15 --baud 115200

依存: pyserial (`pip install pyserial`)
"""

import argparse

import serial


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", default="COM15")
    parser.add_argument("--baud", type=int, default=115200)
    args = parser.parse_args()

    port = serial.Serial(args.port, args.baud, timeout=0.2)
    print(f"pong_bot on {args.port} @{args.baud}", flush=True)
    buf = b""
    try:
        while True:
            data = port.read(256)
            if not data:
                continue
            buf += data
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                print("RX:", line, flush=True)
                if line.strip() == b"PING":
                    port.write(b"PONG 42\n")
                    print("TX: PONG 42", flush=True)
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
