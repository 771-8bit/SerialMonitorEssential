#!/usr/bin/env python3
"""
SerialMonitorEssential Memory Monitor (Python版)

プロセスのメモリ使用量を定期的に記録します。

使用方法:
    python monitor_memory.py --duration 60
    python monitor_memory.py --duration 60 --interval 10
"""

import argparse
import csv
import time
import sys
from datetime import datetime, timedelta
from pathlib import Path
import psutil


def find_process(process_name="serial-monitor-essential"):
    """プロセス名でプロセスを検索"""
    for proc in psutil.process_iter(['pid', 'name']):
        try:
            if process_name.lower() in proc.info['name'].lower():
                return proc
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            pass
    return None


def get_process_role(proc, is_main=False):
    """プロセスの役割を特定する"""
    if is_main:
        return "Backend (Main)"
    
    try:
        name = proc.name()
        cmdline = proc.cmdline()
        cmdline_str = " ".join(cmdline).lower()
        
        if "msedgewebview2" in name.lower():
            if "--type=renderer" in cmdline_str:
                return "Frontend (Renderer)"
            elif "--type=gpu-process" in cmdline_str:
                return "Frontend (GPU)"
            elif "--type=utility" in cmdline_str:
                return "WebView Utility"
            elif "--type=crashpad-handler" in cmdline_str:
                return "Crash Handler"
            elif "--extension-process" in cmdline_str:
                return "Extension"
            else:
                return "WebView Process"
        
        return "Child Process"
    except (psutil.NoSuchProcess, psutil.AccessDenied):
        return "Unknown"


def format_mb(bytes_value):
    """バイトをMBに変換してフォーマット"""
    return round(bytes_value / (1024 * 1024), 2)


def main():
    parser = argparse.ArgumentParser(
        description='Monitor SerialMonitorEssential memory usage'
    )
    parser.add_argument(
        '--process',
        type=str,
        default='serial-monitor-essential',
        help='Process name to monitor (default: serial-monitor-essential)'
    )
    parser.add_argument(
        '--duration',
        type=int,
        default=60,
        help='Duration in minutes (default: 60)'
    )
    parser.add_argument(
        '--interval',
        type=int,
        default=10,
        help='Sampling interval in seconds (default: 10)'
    )
    parser.add_argument(
        '--output',
        '--o',
        type=str,
        default=None,
        help='Output CSV file (default: memory_log_YYYYMMDD_HHMMSS.csv)'
    )
    parser.add_argument(
        '--detailed',
        action='store_true',
        help='Show detailed memory usage per process'
    )
    
    args = parser.parse_args()
    
    # 出力ファイル名
    if args.output is None:
        timestamp = datetime.now().strftime('%Y%m%d_%H%M%S')
        output_file = f"test_results/memory_log_{timestamp}.csv"
    else:
        output_file = args.output
    
    print("=" * 60)
    print("SerialMonitorEssential Memory Monitor")
    print("=" * 60)
    print(f"Process Name: {args.process}")
    print(f"Duration: {args.duration} minutes")
    print(f"Interval: {args.interval} seconds")
    print(f"Output File: {output_file}")
    print()
    
    # プロセスを検索（見つかるまで待機）
    proc = find_process(args.process)
    
    if proc is None:
        print(f"Waiting for process '{args.process}' to start...")
        print("(Press Ctrl+C to cancel)")
        print()
        
        try:
            while proc is None:
                time.sleep(2)  # 2秒ごとにチェック
                proc = find_process(args.process)
        except KeyboardInterrupt:
            print("\n\nMonitoring cancelled by user")
            return 0
    
    print(f"✓ Found process: {proc.name()} (PID: {proc.pid})")
    print()
    print("Monitoring started...")
    print()
    
    # CSV準備
    csv_file = open(output_file, 'w', newline='')
    csv_writer = csv.writer(csv_file)
    csv_writer.writerow(['Timestamp', 'Total_WorkingSet(MB)', 'Total_PrivateBytes(MB)', 'Total_VirtualMemory(MB)', 'Process_Count'])
    
    start_time = datetime.now()
    end_time = start_time + timedelta(minutes=args.duration)
    sample_count = 0
    
    ws_values = []
    pb_values = []
    
    try:
        while datetime.now() < end_time:
            try:
                # メモリ情報取得 (子プロセス含む)
                children = proc.children(recursive=True)
                
                total_rss = proc.memory_info().rss
                try:
                    total_private = proc.memory_info().private
                except AttributeError:
                    # Windowsではprivateが取得できない場合があるためrss等で代用等はせず、0またはrssを使う等の判断が必要だが
                    # psutilのバージョンによってはprivateが無い場合もある。
                    # しかしWindows環境前提で前のコードでprivate使っていたのでそのまま行く。
                    # 万が一取得できない場合は0にするなど安全策をとる。
                    total_private = getattr(proc.memory_info(), 'private', 0)
                
                total_vms = proc.memory_info().vms
                
                child_count = 0
                details = []
                
                # Main process details
                rss_mb = format_mb(proc.memory_info().rss)
                details.append(f"  [{get_process_role(proc, is_main=True)}] PID: {proc.pid}, WS: {rss_mb} MB")

                for child in children:
                    try:
                        mem = child.memory_info()
                        total_rss += mem.rss
                        total_private += getattr(mem, 'private', 0)
                        total_vms += mem.vms
                        child_count += 1
                        
                        role = get_process_role(child)
                        details.append(f"  [{role}] PID: {child.pid}, WS: {format_mb(mem.rss)} MB")
                    except (psutil.NoSuchProcess, psutil.AccessDenied):
                        pass

                ws = format_mb(total_rss)  # Working Set
                pb = format_mb(total_private)  # Private Bytes
                vm = format_mb(total_vms)  # Virtual Memory
                
                # CSV書き込み
                current_time = datetime.now().strftime('%Y-%m-%d %H:%M:%S')
                csv_writer.writerow([current_time, ws, pb, vm, child_count + 1])
                csv_file.flush()
                
                # コンソール出力
                elapsed = (datetime.now() - start_time).total_seconds() / 60
                elapsed_min = int(elapsed)
                elapsed_sec = int((elapsed % 1) * 60)
                print(f"[{elapsed_min:02d}:{elapsed_sec:02d}] Total WS: {ws} MB, Private: {pb} MB (Procs: {child_count + 1})")
                
                if args.detailed:
                    print("\n".join(details))
                    print("-" * 40)
                
                # 統計用に保存
                ws_values.append(ws)
                pb_values.append(pb)
                sample_count += 1
                
                time.sleep(args.interval)
                
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                print(f"Warning: Process terminated or access denied")
                break
    
    except KeyboardInterrupt:
        print("\n\nMonitoring interrupted by user")
    
    finally:
        csv_file.close()
    
    # 統計情報
    print()
    print("Monitoring completed!")
    print(f"Total samples: {sample_count}")
    print(f"Results saved to: {output_file}")
    print()
    
    if sample_count > 0:
        ws_min = min(ws_values)
        ws_max = max(ws_values)
        ws_avg = sum(ws_values) / len(ws_values)
        
        pb_min = min(pb_values)
        pb_max = max(pb_values)
        pb_avg = sum(pb_values) / len(pb_values)
        
        print("=" * 60)
        print("Memory Statistics")
        print("=" * 60)
        print()
        print("Working Set:")
        print(f"  Min: {ws_min:.2f} MB")
        print(f"  Max: {ws_max:.2f} MB")
        print(f"  Avg: {ws_avg:.2f} MB")
        print(f"  Growth: {ws_max - ws_min:.2f} MB")
        print()
        print("Private Bytes:")
        print(f"  Min: {pb_min:.2f} MB")
        print(f"  Max: {pb_max:.2f} MB")
        print(f"  Avg: {pb_avg:.2f} MB")
        print(f"  Growth: {pb_max - pb_min:.2f} MB")
        print()
        
        # メモリリーク検出
        if sample_count > 6:
            mid_point = sample_count // 2
            first_half_avg = sum(ws_values[:mid_point]) / mid_point
            second_half_avg = sum(ws_values[mid_point:]) / (sample_count - mid_point)
            growth_rate = second_half_avg - first_half_avg
            
            if growth_rate > 10:
                print(f"⚠ WARNING: Potential memory leak detected!")
                print(f"  Growth rate: {growth_rate:.2f} MB")
            else:
                print(f"✓ Memory usage appears stable.")
                print(f"  Growth rate: {growth_rate:.2f} MB")
    
    return 0


if __name__ == '__main__':
    sys.exit(main())
