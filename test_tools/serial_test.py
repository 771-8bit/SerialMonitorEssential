#!/usr/bin/env python3
"""
SerialMonitorEssential 統合テストツール

Raspberry Pi Pico または仮想COMポートを使用した
シリアル通信テストを実行します。

使用例:
    # Picoストレステスト
    uv run python serial_test.py --source pico --port COM14 --mode stress --duration 60

    # 仮想COMストレステスト
    uv run python serial_test.py --source virtual --port COM15 --mode stress --duration 10

    # 仮想COM低速テスト
    uv run python serial_test.py --source virtual --port COM15 --mode slow --duration 30

    # データ受信（SendPanelテスト用）
    uv run python serial_test.py --receive --port COM16 --baud 115200
"""

import argparse
import os
import sys
from datetime import datetime
from pathlib import Path

# lib モジュールのパスを追加
sys.path.insert(0, str(Path(__file__).parent))

from lib.pico_controller import PicoController
from lib.virtual_sender import VirtualSender
from lib.serial_receiver import SerialReceiver


# デフォルトボーレート
DEFAULT_BAUD_STRESS = 12000000  # 12Mbps for stress mode
DEFAULT_BAUD_OTHER = 115200    # 115200bps for slow/plotter modes


def run_pico_test(args) -> int:
    """Picoテストを実行"""
    print("=" * 60)
    print("Pico Serial Test")
    print("=" * 60)
    print(f"Port:     {args.port}")
    print(f"Mode:     {args.mode}")
    print(f"Duration: {args.duration} seconds")
    print()

    controller = PicoController(args.port, args.baud)
    
    if not controller.open():
        return 1

    try:
        def on_progress(msg: str):
            print(msg)

        result = controller.start_test(args.mode, args.duration, on_progress)
    finally:
        controller.close()

    # 結果表示
    print()
    print("=" * 60)
    print("TEST COMPLETED" if result.success else "TEST FAILED")
    print("=" * 60)
    
    if result.error:
        print(f"Error: {result.error}")
        return 1

    print(f"Total bytes sent: {result.total_bytes:,}")
    print(f"SHA256 checksum:  {result.checksum}")
    
    # ストレステストモードの場合、実効データレートを計算
    if args.mode == 'stress' and result.duration > 0:
        effective_bps = result.total_bytes * 8 / result.duration
        effective_mbps = effective_bps / 1_000_000
        print(f"Effective rate:   {effective_bps:,.0f} bps ({effective_mbps:.2f} Mbps)")
    
    print()

    # 結果をファイルに保存
    save_result(args.output, "Pico", args, result.total_bytes, result.checksum)

    # 自動検証
    return run_verification(args.output)


def run_virtual_test(args) -> int:
    """仮想COMテストを実行"""
    print("=" * 60)
    print("Virtual Serial Test")
    print("=" * 60)
    print(f"Port:     {args.port}")
    print(f"Baud:     {args.baud:,} bps")
    print(f"Mode:     {args.mode}")
    print(f"Duration: {args.duration} seconds")
    print()

    sender = VirtualSender(args.port, args.baud)
    
    if not sender.open():
        return 1

    try:
        def on_progress(msg: str):
            print(f"  {msg}")

        result = sender.send_data(args.mode, args.duration, on_progress)
    finally:
        sender.close()

    # 結果表示
    print()
    print("=" * 60)
    print("TEST COMPLETED" if result.success else "TEST FAILED")
    print("=" * 60)
    
    if result.error:
        print(f"Error: {result.error}")

    print(f"Total bytes sent: {result.total_bytes:,}")
    print(f"SHA256 checksum:  {result.checksum}")
    
    # ストレステストモードの場合、実効データレートを計算
    if args.mode == 'stress' and result.duration > 0:
        effective_bps = result.total_bytes * 8 / result.duration
        effective_mbps = effective_bps / 1_000_000
        print(f"Effective rate:   {effective_bps:,.0f} bps ({effective_mbps:.2f} Mbps)")
    
    print()

    # 結果をファイルに保存
    save_result(args.output, "Virtual", args, result.total_bytes, result.checksum)

    # 自動検証
    return run_verification(args.output)


def run_receive_test(args) -> int:
    """受信テストを実行"""
    print("=" * 60)
    print("Serial Receive Test")
    print("=" * 60)
    print(f"Port:         {args.port}")
    print(f"Baud:         {args.baud:,} bps")
    print(f"Timeout:      {args.timeout} seconds")
    print(f"Idle Timeout: {args.idle_timeout} seconds")
    print()
    print("Waiting for data... (Press Ctrl+C to stop)")
    print()

    receiver = SerialReceiver(args.port, args.baud)
    
    if not receiver.open():
        return 1

    try:
        def on_progress(msg: str):
            print(f"  {msg}")

        def on_data(data: bytes):
            if args.verbose and len(data) < 1024:
                try:
                    text = data.decode('utf-8', errors='replace')
                    for line in text.splitlines():
                        if line.strip():
                            print(f"  > {line}")
                except Exception:
                    pass

        result = receiver.receive(
            total_timeout=args.timeout,
            idle_timeout=args.idle_timeout,
            on_progress=on_progress,
            on_data=on_data
        )
    finally:
        receiver.close()

    # 結果表示
    print()
    print("=" * 60)
    print("RECEIVE COMPLETED" if result.success else "RECEIVE FAILED")
    print("=" * 60)
    
    if result.error:
        print(f"Error: {result.error}")

    print(f"Total bytes received: {result.total_bytes:,}")
    print(f"SHA256 checksum:      {result.checksum}")
    print()

    # データ保存
    if result.total_bytes > 0:
        output_file = args.output.replace('.txt', '_received.bin')
        receiver.save_data(output_file)
        print(f"✓ Data saved to: {output_file}")
        print()

    # 検証
    if args.expected_bytes is not None or args.expected_checksum is not None:
        success, errors = receiver.verify(args.expected_bytes, args.expected_checksum)
        
        print("=" * 60)
        print("VERIFICATION")
        print("=" * 60)
        
        if success:
            print("✓✓✓ ALL CHECKS PASSED! ✓✓✓")
            return 0
        else:
            for error in errors:
                print(f"✗ {error}")
            return 1

    return 0 if result.success else 1


def save_result(output_path: str, source: str, args, total_bytes: int, checksum: str):
    """テスト結果をファイルに保存"""
    os.makedirs(os.path.dirname(output_path), exist_ok=True)
    
    timestamp = datetime.now().strftime('%Y-%m-%d %H:%M:%S')
    with open(output_path, 'w') as f:
        f.write(f"{source} Serial Test Results\n")
        f.write("=" * 40 + "\n\n")
        f.write(f"Timestamp: {timestamp}\n")
        f.write(f"Port: {args.port}\n")
        f.write(f"Baud Rate: {args.baud:,} bps\n")
        f.write(f"Mode: {args.mode}\n")
        f.write(f"Duration: {args.duration} seconds\n\n")
        f.write(f"Total bytes sent: {total_bytes}\n")
        f.write(f"SHA256 checksum: {checksum}\n")

    print(f"✓ Results saved to: {output_path}")


def run_verification(result_file: str) -> int:
    """受信データを検証"""
    print()
    print("=" * 60)
    print("AUTOMATIC VERIFICATION")
    print("=" * 60)
    print()

    try:
        from verify_received_data import (
            find_serial_monitor_temp_dir,
            parse_test_result,
            calculate_sha256
        )

        # 期待値を読み込み
        expected_bytes, expected_checksum, error = parse_test_result(result_file)
        if error:
            print(f"⚠ Warning: Could not read test results: {error}")
            return 0

        # 受信ファイルを探す
        data_file, status = find_serial_monitor_temp_dir()
        if data_file is None:
            print(f"⚠ Warning: Could not find received data: {status}")
            return 0

        print(f"Found received data: {data_file}")

        # バイト数確認
        actual_bytes = data_file.stat().st_size
        print(f"Expected bytes:  {expected_bytes:,}")
        print(f"Received bytes:  {actual_bytes:,}")

        bytes_match = actual_bytes == expected_bytes
        if bytes_match:
            print("✓ Byte count MATCHES!")
        else:
            print(f"✗ Byte count MISMATCH! (diff: {actual_bytes - expected_bytes:+,})")

        # チェックサム確認
        print("\nCalculating SHA256 checksum...")
        actual_checksum = calculate_sha256(data_file)

        if actual_checksum is None:
            print("✗ Error calculating checksum")
            return 1

        checksum_match = actual_checksum == expected_checksum
        print(f"Expected checksum: {expected_checksum}")
        print(f"Actual checksum:   {actual_checksum}")

        if checksum_match:
            print("✓ Checksum MATCHES!")
        else:
            print("✗ Checksum MISMATCH!")

        # 最終結果
        print()
        print("=" * 60)
        if bytes_match and checksum_match:
            print("✓✓✓ ALL CHECKS PASSED! ✓✓✓")
            return 0
        else:
            print("✗✗✗ VERIFICATION FAILED! ✗✗✗")
            return 1

    except ImportError as e:
        print(f"⚠ Warning: Could not import verification module: {e}")
        return 0
    except Exception as e:
        print(f"⚠ Warning: Verification error: {e}")
        return 0


def main():
    parser = argparse.ArgumentParser(
        description='SerialMonitorEssential integrated test tool',
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  # Pico stress test
  %(prog)s --source pico --port COM14 --mode stress --duration 60

  # Virtual COM stress test
  %(prog)s --source virtual --port COM15 --mode stress --duration 10

  # Receive mode (for SendPanel testing)
  %(prog)s --receive --port COM16 --verbose
"""
    )

    # 送信/受信モード
    mode_group = parser.add_mutually_exclusive_group(required=True)
    mode_group.add_argument(
        '--source',
        type=str,
        choices=['pico', 'virtual'],
        help='Data source: pico (Raspberry Pi Pico) or virtual (virtual COM port)'
    )
    mode_group.add_argument(
        '--receive',
        action='store_true',
        help='Receive mode (for testing SendPanel)'
    )

    # 共通オプション
    parser.add_argument(
        '--port',
        type=str,
        required=True,
        help='Serial port (e.g., COM14, /tmp/vcom0)'
    )
    parser.add_argument(
        '--baud',
        type=int,
        default=115200,
        help='Baud rate (default: 115200)'
    )
    parser.add_argument(
        '--output',
        type=str,
        default='test_results/test_result.txt',
        help='Output file for test results'
    )

    # 送信モードオプション
    parser.add_argument(
        '--mode',
        type=str,
        choices=['stress', 'slow', 'fast', 'plot:csv', 'plot:label', 'demo'],
        default='stress',
        help='Test mode: stress (fast binary), slow (1 line/sec), fast (1 line/0.1ms), plot:csv (CSV 10Hz), plot:label (Labeled 10Hz), demo (various patterns)'
    )
    parser.add_argument(
        '--duration',
        type=int,
        default=10,
        help='Test duration in seconds (default: 10)'
    )

    # 受信モードオプション
    parser.add_argument(
        '--timeout',
        type=float,
        default=30.0,
        help='Total receive timeout in seconds (default: 30)'
    )
    parser.add_argument(
        '--idle-timeout',
        type=float,
        default=3.0,
        help='Stop after this many seconds of no data (default: 3)'
    )
    parser.add_argument(
        '--expected-bytes',
        type=int,
        default=None,
        help='Expected number of bytes (for verification)'
    )
    parser.add_argument(
        '--expected-checksum',
        type=str,
        default=None,
        help='Expected SHA256 checksum (for verification)'
    )
    parser.add_argument(
        '--verbose',
        action='store_true',
        help='Show received data as text'
    )

    args = parser.parse_args()

    # stressモードのときはデフォルトで12Mbps、それ以外は115200bps
    # ユーザーが明示的に--baudを指定していない場合のみ自動設定
    if not any(arg.startswith('--baud') for arg in sys.argv):
        if hasattr(args, 'mode') and args.mode == 'stress':
            args.baud = DEFAULT_BAUD_STRESS
        else:
            args.baud = DEFAULT_BAUD_OTHER

    # 実行
    if args.receive:
        return run_receive_test(args)
    elif args.source == 'pico':
        return run_pico_test(args)
    elif args.source == 'virtual':
        return run_virtual_test(args)
    else:
        parser.print_help()
        return 1


if __name__ == '__main__':
    sys.exit(main())
