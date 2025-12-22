#!/usr/bin/env python3
"""
Raspberry Pi Pico プロッタテストコントローラー

Picoに対してプロッタテスト開始コマンドを送信し、
CSVフォーマットの数値データを10Hzで送信します。
シリアルプロッタ機能のテストに使用します。

使用方法:
    python pico_plotter_test_controller.py --port COM3 --duration 30
"""

import argparse
import serial
import time
import sys


def main():
    parser = argparse.ArgumentParser(
        description='Raspberry Pi Pico plotter test controller (10Hz CSV data)'
    )
    parser.add_argument(
        '--port',
        type=str,
        required=True,
        help='Pico control serial port (e.g., COM14)'
    )
    parser.add_argument(
        '--duration',
        type=int,
        default=30,
        help='Test duration in seconds (default: 30)'
    )
    parser.add_argument(
        '--baud',
        type=int,
        default=115200,
        help='Baud rate for control port (default: 115200)'
    )
    
    args = parser.parse_args()
    
    print(f"=== Pico Plotter Test Controller ===")
    print(f"Control Port: {args.port}")
    print(f"Baud Rate: {args.baud} bps")
    print(f"Duration: {args.duration} seconds")
    print()
    print("This test sends CSV data at 10Hz for serial plotter testing.")
    print("Data format: timestamp,sin,cos,random_walk")
    print()
    
    try:
        print(f"Opening {args.port}...")
        ser = serial.Serial(args.port, args.baud, timeout=1)
        time.sleep(2)  # Picoのリセット待機
        print("Port opened successfully!")
    except serial.SerialException as e:
        print(f"Error: Failed to open port: {e}", file=sys.stderr)
        return 1
    
    # 既存の出力をクリア
    while ser.in_waiting:
        ser.read(ser.in_waiting)
    
    # プロッタテスト開始コマンド送信
    command = f"PLOTTER:{args.duration}\n"
    print(f"\nSending command: {command.strip()}")
    ser.write(command.encode())
    ser.flush()
    
    # Picoからの応答待機
    print("Waiting for Pico response...")
    start_time = time.time()
    test_started = False
    total_bytes = 0
    lines_sent = 0
    
    try:
        while True:
            if ser.in_waiting:
                line = ser.readline().decode('utf-8', errors='ignore').strip()
                
                if not line:
                    continue
                
                # 進捗表示をフィルタリング
                if line.startswith("Plotter:"):
                    parts = line.split()
                    if len(parts) >= 2:
                        lines_sent = int(parts[1])
                    print(f"  [Progress] {line}")
                else:
                    print(f"[Pico] {line}")
                
                if line == "PLOTTER_TEST_START":
                    test_started = True
                    print("\n✓ Plotter test started on Pico")
                    print(f"Sending CSV data at 10Hz for {args.duration} seconds...")
                    print(f"Expected lines: ~{args.duration * 10}")
                    print()
                
                elif line == "TEST_STOP":
                    print("\n✓ Test completed on Pico")
                
                elif line.startswith("Total bytes:"):
                    total_bytes = int(line.split(":")[1].strip())
                    print(f"  Total bytes sent: {total_bytes:,}")
                
                elif line.startswith("Checksum:"):
                    checksum = line.split(":")[1].strip()
                    print(f"  SHA256 checksum: {checksum}")
                    break  # テスト完了
                
            # タイムアウトチェック（duration + 10秒のバッファ）
            if time.time() - start_time > args.duration + 10:
                print("\nWarning: Timeout waiting for test completion", file=sys.stderr)
                break
            
            time.sleep(0.1)
    
    except KeyboardInterrupt:
        print("\n\nTest interrupted by user")
        ser.write(b"STOP\n")
        ser.flush()
    
    ser.close()
    
    if not test_started:
        print("\nError: Test did not start properly", file=sys.stderr)
        print("Make sure the Pico firmware is updated with PLOTTER command support.")
        return 1
    
    print("\n" + "=" * 60)
    print("PLOTTER TEST RESULTS")
    print("=" * 60)
    print(f"Lines sent: {lines_sent}")
    print(f"Total bytes: {total_bytes:,}")
    print(f"Expected lines: ~{args.duration * 10}")
    expected_bytes = args.duration * 10 * 30  # 約30バイト/行
    print(f"Expected bytes: ~{expected_bytes:,} (rough estimate)")
    print()
    
    if lines_sent > 0:
        print("✓ Plotter test completed successfully!")
        print()
        print("CSV data format sent to data port:")
        print("  timestamp,sin_wave,cos_wave,random_walk")
        print()
        print("Now check the SerialMonitorEssential UI:")
        print("  - Did the data viewer show CSV formatted data?")
        print("  - Is the data suitable for plotting?")
    else:
        print("✗ No lines were sent - check the Pico firmware")
    
    print("=" * 60)
    return 0


if __name__ == '__main__':
    sys.exit(main())
