#!/usr/bin/env python3
"""
Raspberry Pi Pico ストレステストコントローラー

Picoに対してテスト開始コマンドを送信し、
テスト結果（送信バイト数、チェックサム）を受信・記録します。

使用方法:
    python pico_stress_test_controller.py --port COM3 --duration 60
"""

import argparse
import serial
import time
import sys
from datetime import datetime


def main():
    parser = argparse.ArgumentParser(
        description='Raspberry Pi Pico stress test controller'
    )
    parser.add_argument(
        '--port',
        type=str,
        required=True,
        help='Pico serial port (e.g., COM3)'
    )
    parser.add_argument(
        '--duration',
        type=int,
        default=60,
        help='Test duration in seconds (default: 60)'
    )
    parser.add_argument(
        '--baud',
        type=int,
        default=12000000,
        help='Baud rate (default: 12000000, must match Pico firmware)'
    )
    parser.add_argument(
        '--output',
        type=str,
        default='test_results/test_result.txt',
        help='Output file for test results (default: test_results/test_result.txt)'
    )
    
    args = parser.parse_args()
    
    print(f"=== Pico Stress Test Controller ===")
    print(f"Port: {args.port}")
    print(f"Baud Rate: {args.baud:,} bps")
    print(f"Duration: {args.duration} seconds")
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
    
    # テスト開始コマンド送信
    command = f"START:{args.duration}\n"
    print(f"\nSending command: {command.strip()}")
    ser.write(command.encode())
    ser.flush()
    
    # Picoからの応答待機
    print("Waiting for Pico response...")
    start_time = time.time()
    test_started = False
    total_bytes = 0
    checksum = ""
    
    try:
        while True:
            if ser.in_waiting:
                line = ser.readline().decode('utf-8', errors='ignore').strip()
                
                if not line:
                    continue
                
                print(f"[Pico] {line}")
                
                if line == "TEST_START":
                    test_started = True
                    print("\n✓ Test started on Pico")
                    print(f"Waiting for {args.duration} seconds...")
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
        return 1
    
    if total_bytes == 0:
        print("\nError: No test results received", file=sys.stderr)
        return 1
    
    # 結果をファイルに保存
    timestamp = datetime.now().strftime('%Y-%m-%d %H:%M:%S')
    
    with open(args.output, 'w') as f:
        f.write(f"Raspberry Pi Pico Stress Test Results\n")
        f.write(f"======================================\n\n")
        f.write(f"Timestamp: {timestamp}\n")
        f.write(f"Port: {args.port}\n")
        f.write(f"Baud Rate: {args.baud:,} bps\n")
        f.write(f"Duration: {args.duration} seconds\n\n")
        f.write(f"Total bytes sent: {total_bytes}\n")
        f.write(f"SHA256 checksum: {checksum}\n")
    
    print(f"\n✓ Results saved to: {args.output}")
    print("\n" + "=" * 60)
    print("AUTOMATIC VERIFICATION")
    print("=" * 60)
    print("\nVerifying received data automatically...")
    print("(This may take a moment for large files...)\n")
    
    # 自動検証を実行
    try:
        # verify_received_data モジュールをインポート
        import os
        sys.path.insert(0, os.path.dirname(__file__))
        from verify_received_data import (
            find_serial_monitor_temp_dir,
            parse_test_result,
            calculate_sha256
        )
        
        # 期待値を読み込み
        expected_bytes, expected_checksum, error = parse_test_result(args.output)
        if error:
            print(f"⚠ Warning: Could not read test results for verification: {error}")
            print("  You can manually verify using: uv run python verify_received_data.py")
            return 0
        
        # 受信ファイルを探す
        data_file, status = find_serial_monitor_temp_dir()
        if data_file is None:
            print(f"⚠ Warning: Could not find received data file: {status}")
            print("  Make sure SerialMonitorEssential is still running or was recently closed.")
            print("  You can manually verify using: uv run python verify_received_data.py")
            return 0
        
        print(f"Found received data: {data_file}")
        
        # バイト数確認
        actual_bytes = data_file.stat().st_size
        print(f"Expected bytes:  {expected_bytes:,}")
        print(f"Received bytes:  {actual_bytes:,}")
        
        bytes_match = actual_bytes == expected_bytes
        if bytes_match:
            print("✓ Byte count MATCHES!\n")
        else:
            print(f"✗ Byte count MISMATCH! (Difference: {actual_bytes - expected_bytes:+,} bytes)\n")
        
        # チェックサム確認
        print("Calculating SHA256 checksum...")
        actual_checksum = calculate_sha256(data_file)
        
        if actual_checksum is None:
            print("✗ Error calculating checksum")
            return 1
        
        checksum_match = actual_checksum == expected_checksum
        print(f"Expected checksum: {expected_checksum}")
        print(f"Actual checksum:   {actual_checksum}")
        
        if checksum_match:
            print("✓ Checksum MATCHES!\n")
        else:
            print("✗ Checksum MISMATCH!\n")
        
        # 最終結果
        print("=" * 60)
        if bytes_match and checksum_match:
            print("✓✓✓ ALL CHECKS PASSED! ✓✓✓")
            print("\nThe received data is CORRECT!")
            print(f"  Bytes received: {actual_bytes:,}")
            print(f"  Data integrity: 100%")
            print("=" * 60)
            return 0
        else:
            print("✗✗✗ VERIFICATION FAILED! ✗✗✗")
            if not bytes_match:
                print(f"  ✗ Byte count mismatch")
                loss_rate = abs(actual_bytes - expected_bytes) / expected_bytes * 100
                print(f"    Data loss rate: {loss_rate:.2f}%")
            if not checksum_match:
                print(f"  ✗ Checksum mismatch (data corruption)")
            print("=" * 60)
            return 1
            
    except ImportError as e:
        print(f"⚠ Warning: Could not import verification module: {e}")
        print("  You can manually verify using: uv run python verify_received_data.py")
        return 0
    except Exception as e:
        print(f"⚠ Warning: Verification error: {e}")
        print("  You can manually verify using: uv run python verify_received_data.py")
        return 0


if __name__ == '__main__':
    sys.exit(main())
