#!/usr/bin/env python3
"""
SerialMonitorEssential メモリ使用量分析スクリプト

monitor_memory.ps1 で記録したCSVファイルを分析し、
グラフ化と統計情報を出力する。

使用方法:
    python analyze_memory.py memory_log_20231219_143052.csv
"""

import argparse
import sys
import pandas as pd
import matplotlib.pyplot as plt
import matplotlib.dates as mdates
from pathlib import Path


def analyze_memory(csv_file: str, output_dir: str = "."):
    """メモリログを分析してグラフと統計情報を生成
    
    Args:
        csv_file: 入力CSVファイルパス
        output_dir: 出力ディレクトリ
    """
    # CSVファイル読み込み
    try:
        df = pd.read_csv(csv_file)
        df['Timestamp'] = pd.to_datetime(df['Timestamp'])
    except Exception as e:
        print(f"Error: Failed to read CSV file: {e}", file=sys.stderr)
        return 1
    
    if len(df) == 0:
        print("Error: No data in CSV file", file=sys.stderr)
        return 1
    
    # グラフ作成
    fig, (ax1, ax2) = plt.subplots(2, 1, figsize=(14, 10))
    
    # Working Set と Private Bytes
    ax1.plot(df['Timestamp'], df['WorkingSet(MB)'], 
             label='Working Set', linewidth=2, marker='o', markersize=4)
    ax1.plot(df['Timestamp'], df['PrivateBytes(MB)'], 
             label='Private Bytes', linewidth=2, marker='s', markersize=4)
    
    if 'VirtualMemory(MB)' in df.columns:
        ax1.plot(df['Timestamp'], df['VirtualMemory(MB)'], 
                 label='Virtual Memory', linewidth=2, marker='^', markersize=4, alpha=0.7)
    
    ax1.set_xlabel('Time', fontsize=12)
    ax1.set_ylabel('Memory (MB)', fontsize=12)
    ax1.set_title('SerialMonitorEssential Memory Usage Over Time', fontsize=14, fontweight='bold')
    ax1.legend(loc='upper left', fontsize=10)
    ax1.grid(True, alpha=0.3)
    ax1.xaxis.set_major_formatter(mdates.DateFormatter('%H:%M:%S'))
    plt.setp(ax1.xaxis.get_majorticklabels(), rotation=45, ha='right')
    
    # メモリ増加率（微分）
    df['WS_Diff'] = df['WorkingSet(MB)'].diff()
    df['PB_Diff'] = df['PrivateBytes(MB)'].diff()
    
    ax2.plot(df['Timestamp'], df['WS_Diff'], 
             label='Working Set Growth', linewidth=1.5, alpha=0.7)
    ax2.plot(df['Timestamp'], df['PB_Diff'], 
             label='Private Bytes Growth', linewidth=1.5, alpha=0.7)
    ax2.axhline(y=0, color='red', linestyle='--', linewidth=1, alpha=0.5)
    
    ax2.set_xlabel('Time', fontsize=12)
    ax2.set_ylabel('Memory Change (MB/sample)', fontsize=12)
    ax2.set_title('Memory Growth Rate', fontsize=14, fontweight='bold')
    ax2.legend(loc='upper left', fontsize=10)
    ax2.grid(True, alpha=0.3)
    ax2.xaxis.set_major_formatter(mdates.DateFormatter('%H:%M:%S'))
    plt.setp(ax2.xaxis.get_majorticklabels(), rotation=45, ha='right')
    
    plt.tight_layout()
    
    # グラフ保存
    output_path = Path(output_dir)
    output_path.mkdir(parents=True, exist_ok=True)
    
    graph_file = output_path / 'memory_analysis.png'
    plt.savefig(graph_file, dpi=150, bbox_inches='tight')
    print(f"Graph saved to: {graph_file}")
    
    # 統計情報
    print("\n" + "="*60)
    print("Memory Usage Statistics")
    print("="*60 + "\n")
    
    print(f"Total Samples: {len(df)}")
    print(f"Duration: {(df['Timestamp'].iloc[-1] - df['Timestamp'].iloc[0]).total_seconds():.0f} seconds")
    print()
    
    print("Working Set (MB):")
    print(f"  Min:    {df['WorkingSet(MB)'].min():.2f}")
    print(f"  Max:    {df['WorkingSet(MB)'].max():.2f}")
    print(f"  Mean:   {df['WorkingSet(MB)'].mean():.2f}")
    print(f"  StdDev: {df['WorkingSet(MB)'].std():.2f}")
    print(f"  Growth: {df['WorkingSet(MB)'].iloc[-1] - df['WorkingSet(MB)'].iloc[0]:.2f}")
    print()
    
    print("Private Bytes (MB):")
    print(f"  Min:    {df['PrivateBytes(MB)'].min():.2f}")
    print(f"  Max:    {df['PrivateBytes(MB)'].max():.2f}")
    print(f"  Mean:   {df['PrivateBytes(MB)'].mean():.2f}")
    print(f"  StdDev: {df['PrivateBytes(MB)'].std():.2f}")
    print(f"  Growth: {df['PrivateBytes(MB)'].iloc[-1] - df['PrivateBytes(MB)'].iloc[0]:.2f}")
    print()
    
    # リーク判定
    if len(df) > 10:
        first_half = df.iloc[:len(df)//2]
        second_half = df.iloc[len(df)//2:]
        
        ws_growth = second_half['WorkingSet(MB)'].mean() - first_half['WorkingSet(MB)'].mean()
        pb_growth = second_half['PrivateBytes(MB)'].mean() - first_half['PrivateBytes(MB)'].mean()
        
        print("Memory Leak Analysis:")
        print(f"  Working Set growth (1st half -> 2nd half): {ws_growth:.2f} MB")
        print(f"  Private Bytes growth (1st half -> 2nd half): {pb_growth:.2f} MB")
        print()
        
        if ws_growth > 10 or pb_growth > 10:
            print("  ⚠️  WARNING: Potential memory leak detected!")
            print("      Memory usage increased significantly during the test.")
        else:
            print("  ✅ Memory usage appears stable.")
        print()
    
    # 統計情報をファイルに保存
    stats_file = output_path / 'memory_statistics.txt'
    with open(stats_file, 'w') as f:
        f.write("SerialMonitorEssential Memory Analysis\n")
        f.write("=" * 60 + "\n\n")
        f.write(f"Input File: {csv_file}\n")
        f.write(f"Total Samples: {len(df)}\n")
        f.write(f"Duration: {(df['Timestamp'].iloc[-1] - df['Timestamp'].iloc[0]).total_seconds():.0f} seconds\n\n")
        
        f.write("Working Set Statistics:\n")
        f.write(df['WorkingSet(MB)'].describe().to_string())
        f.write("\n\nPrivate Bytes Statistics:\n")
        f.write(df['PrivateBytes(MB)'].describe().to_string())
        f.write("\n")
    
    print(f"Statistics saved to: {stats_file}")
    
    return 0


def main():
    parser = argparse.ArgumentParser(
        description='Analyze SerialMonitorEssential memory usage log'
    )
    parser.add_argument(
        'csv_file',
        type=str,
        help='Memory log CSV file to analyze'
    )
    parser.add_argument(
        '--output',
        type=str,
        default='test_results',
        help='Output directory for graphs and statistics (default: test_results)'
    )
    
    args = parser.parse_args()
    
    if not Path(args.csv_file).exists():
        print(f"Error: File not found: {args.csv_file}", file=sys.stderr)
        return 1
    
    return analyze_memory(args.csv_file, args.output)


if __name__ == '__main__':
    sys.exit(main())
