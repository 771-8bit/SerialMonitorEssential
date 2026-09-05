#!/usr/bin/env python3
"""
SerialMonitorEssential 受信データ検証スクリプト

test_result.txt と SerialMonitorEssential で受信したデータを比較し、
バイト数とチェックサムが一致するか検証します。

使用方法:
    python verify_received_data.py
    python verify_received_data.py --result test_result.txt
"""

import argparse
import hashlib
import os
import sys
import glob
import psutil
from pathlib import Path


def find_serial_monitor_temp_dir():
    """SerialMonitorEssentialの一時ディレクトリを探す"""
    temp_base = Path(os.environ.get('TEMP', os.environ.get('TMP', 'C:\\Windows\\Temp')))
    serial_monitor_dir = temp_base / 'SerialMonitorEssential'
    
    if not serial_monitor_dir.exists():
        return None, "SerialMonitorEssential temp directory not found"
    
    # PIDディレクトリを探す
    pid_dirs = list(serial_monitor_dir.glob('*'))
    
    if len(pid_dirs) == 0:
        return None, "No PID directories found"
    
    # 実行中のプロセスを確認
    running_pids = set()
    for proc in psutil.process_iter(['pid', 'name']):
        try:
            if 'tauri' in proc.info['name'].lower() or 'serial' in proc.info['name'].lower():
                running_pids.add(proc.info['pid'])
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            pass
    
    # 最新のPIDディレクトリを探す（実行中のプロセスを優先）
    # 現行アプリは <pid>/<インスタンス番号>/data.bin（ポート再オープンごとに
    # インスタンスが増える）。旧レイアウト <pid>/data.bin も後方互換で探す。
    valid_dirs = []
    for pid_dir in pid_dirs:
        if not pid_dir.is_dir():
            continue

        try:
            pid = int(pid_dir.name)
        except ValueError:
            continue

        candidates = [pid_dir / 'data.bin']  # 旧レイアウト
        candidates += sorted(pid_dir.glob('*/data.bin'))  # 現行: インスタンス別
        for data_file in candidates:
            if data_file.exists():
                is_running = pid in running_pids
                mtime = data_file.stat().st_mtime
                valid_dirs.append((pid_dir, pid, is_running, mtime, data_file))
    
    if len(valid_dirs) == 0:
        return None, "No data.bin files found in any PID directory"
    
    # 実行中のプロセス優先、次に最新のファイル
    valid_dirs.sort(key=lambda x: (not x[2], -x[3]))
    
    best_dir, pid, is_running, mtime, data_file = valid_dirs[0]
    status = "running" if is_running else "not running"
    
    return data_file, f"Found data.bin for PID {pid} ({status})"


def parse_test_result(result_file):
    """test_result.txtから期待値を読み取る"""
    if not os.path.exists(result_file):
        return None, None, f"Result file not found: {result_file}"
    
    expected_bytes = None
    expected_checksum = None
    
    try:
        with open(result_file, 'r', encoding='utf-8') as f:
            for line in f:
                line = line.strip()
                if line.startswith('Total bytes sent:'):
                    expected_bytes = int(line.split(':')[1].strip())
                elif line.startswith('SHA256 checksum:'):
                    expected_checksum = line.split(':')[1].strip().upper()
        
        if expected_bytes is None or expected_checksum is None:
            return None, None, "Could not parse test result file"
        
        return expected_bytes, expected_checksum, None
    
    except Exception as e:
        return None, None, f"Error reading result file: {e}"


def calculate_sha256(file_path):
    """ファイルのSHA256チェックサムを計算"""
    sha256 = hashlib.sha256()
    
    try:
        with open(file_path, 'rb') as f:
            while True:
                chunk = f.read(1024 * 1024)  # 1MB chunks
                if not chunk:
                    break
                sha256.update(chunk)
        
        return sha256.hexdigest().upper()
    except Exception as e:
        return None


def main():
    parser = argparse.ArgumentParser(
        description='Verify SerialMonitorEssential received data'
    )
    parser.add_argument(
        '--result',
        type=str,
        default='test_results/test_result.txt',
        help='Test result file (default: test_results/test_result.txt)'
    )
    
    args = parser.parse_args()
    
    print("=" * 60)
    print("SerialMonitorEssential Data Verification")
    print("=" * 60)
    print()
    
    # 1. 期待値を読み取り
    print("[1/4] Reading test results...")
    expected_bytes, expected_checksum, error = parse_test_result(args.result)
    
    if error:
        print(f"  ✗ Error: {error}")
        return 1
    
    print(f"  Expected bytes:    {expected_bytes:,}")
    print(f"  Expected checksum: {expected_checksum}")
    print()
    
    # 2. 受信ファイルを探す
    print("[2/4] Searching for received data file...")
    data_file, status = find_serial_monitor_temp_dir()
    
    if data_file is None:
        print(f"  ✗ Error: {status}")
        print()
        print("Hint: Make sure SerialMonitorEssential has received data.")
        print("      The data.bin file should be in %TEMP%\\SerialMonitorEssential\\<PID>\\")
        return 1
    
    print(f"  ✓ {status}")
    print(f"  File: {data_file}")
    print()
    
    # 3. バイト数確認
    print("[3/4] Verifying byte count...")
    actual_bytes = data_file.stat().st_size
    print(f"  Actual bytes: {actual_bytes:,}")
    
    bytes_match = actual_bytes == expected_bytes
    if bytes_match:
        print(f"  ✓ Byte count MATCHES!")
    else:
        print(f"  ✗ Byte count MISMATCH!")
        print(f"    Difference: {actual_bytes - expected_bytes:+,} bytes")
    print()
    
    # 4. チェックサム確認
    print("[4/4] Calculating SHA256 checksum...")
    print("  (This may take a moment for large files...)")
    
    actual_checksum = calculate_sha256(data_file)
    
    if actual_checksum is None:
        print(f"  ✗ Error calculating checksum")
        return 1
    
    print(f"  Actual checksum: {actual_checksum}")
    
    checksum_match = actual_checksum == expected_checksum
    if checksum_match:
        print(f"  ✓ Checksum MATCHES!")
    else:
        print(f"  ✗ Checksum MISMATCH!")
    print()
    
    # 結果サマリー
    print("=" * 60)
    print("VERIFICATION RESULT")
    print("=" * 60)
    print()
    
    if bytes_match and checksum_match:
        print("✓✓✓ ALL CHECKS PASSED! ✓✓✓")
        print()
        print("The received data is CORRECT!")
        print("SerialMonitorEssential successfully received all data")
        print("without any loss or corruption.")
        print()
        print(f"  Bytes received: {actual_bytes:,}")
        print(f"  Data integrity: 100%")
        return 0
    else:
        print("✗✗✗ VERIFICATION FAILED! ✗✗✗")
        print()
        
        if not bytes_match:
            print(f"  ✗ Byte count mismatch")
            loss_rate = abs(actual_bytes - expected_bytes) / expected_bytes * 100
            print(f"    Data loss rate: {loss_rate:.2f}%")
        
        if not checksum_match:
            print(f"  ✗ Checksum mismatch")
            print(f"    Data corruption detected!")
        
        print()
        print("Possible causes:")
        print("  - Serial port buffer overflow")
        print("  - USB cable issue")
        print("  - SerialMonitorEssential bug")
        return 1


if __name__ == '__main__':
    sys.exit(main())
