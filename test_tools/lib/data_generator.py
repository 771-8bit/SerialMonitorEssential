#!/usr/bin/env python3
"""
テストデータ生成モジュール

Picoファームウェアと同じデータパターンを生成します。
stress/slow/plotter の各テストモードに対応。
"""

import hashlib
import math
import random
from dataclasses import dataclass
from typing import Iterator


@dataclass
class TestResult:
    """テスト結果"""
    total_bytes: int
    checksum: str
    mode: str
    duration: float


class DataGenerator:
    """テストデータ生成クラス"""

    CHUNK_SIZE = 1024  # 1KB chunks (Picoと同じ)

    def __init__(self):
        self.sha256 = hashlib.sha256()
        self.total_bytes = 0
        self.counter = 0  # カウンタベースのデータ生成用
        self.slow_counter = 0  # 低速テスト用行カウンタ
        self.random_walk = 0.0  # プロッタ用ランダムウォーク

    def reset(self):
        """状態をリセット"""
        self.sha256 = hashlib.sha256()
        self.total_bytes = 0
        self.counter = 0
        self.slow_counter = 0
        self.random_walk = 0.0

    def get_checksum(self) -> str:
        """SHA256チェックサムを取得"""
        return self.sha256.hexdigest().upper()

    def update_checksum(self, data: bytes):
        """チェックサムを更新"""
        self.sha256.update(data)
        self.total_bytes += len(data)

    def generate_stress_chunk(self, size: int = None) -> bytes:
        """
        高速テスト用データチャンク生成（Picoと同じパターン）
        
        カウンタ値 0-255 の繰り返しパターン
        """
        if size is None:
            size = self.CHUNK_SIZE
        
        data = bytes((self.counter + i) % 256 for i in range(size))
        self.counter = (self.counter + size) % 256
        self.update_checksum(data)
        return data

    def generate_slow_line(self, elapsed_sec: int) -> bytes:
        """
        低速テスト用1行生成（Picoと同じフォーマット）
        
        フォーマット: [NNNN] Hello from Virtual Port! Counter=N\r\n
        """
        line = f"[{elapsed_sec:04d}] Hello from Virtual Port! Counter={self.slow_counter}\r\n"
        data = line.encode('utf-8')
        self.slow_counter += 1
        self.update_checksum(data)
        return data

    def generate_plotter_header(self) -> bytes:
        """プロッタテスト用ヘッダー行生成"""
        header = "time,sin,cos,random\r\n"
        data = header.encode('utf-8')
        self.update_checksum(data)
        return data

    def generate_plotter_line(self, elapsed_sec: float) -> bytes:
        """
        プロッタテスト用CSV行生成（Picoと同じフォーマット）
        
        フォーマット: time,sin,cos,random\r\n
        - sin: 周期2秒、振幅100
        - cos: 周期3秒、振幅80
        - random: ランダムウォーク ±150
        """
        # Ch1: Sin波 (周期2秒、振幅100)
        ch1 = 100.0 * math.sin(2.0 * math.pi * elapsed_sec / 2.0)
        
        # Ch2: Cos波 (周期3秒、振幅80)
        ch2 = 80.0 * math.cos(2.0 * math.pi * elapsed_sec / 3.0)
        
        # Ch3: ランダムウォーク
        self.random_walk += (random.random() * 2 - 1) * 5.0
        self.random_walk = max(-150.0, min(150.0, self.random_walk))
        
        line = f"{elapsed_sec:.2f},{ch1:.2f},{ch2:.2f},{self.random_walk:.2f}\r\n"
        data = line.encode('utf-8')
        self.update_checksum(data)
        return data

    def stress_generator(self, duration: float, chunk_size: int = None) -> Iterator[bytes]:
        """
        ストレステスト用データジェネレータ
        
        Args:
            duration: テスト時間（秒）
            chunk_size: チャンクサイズ（省略時は CHUNK_SIZE）
        
        Yields:
            bytes: データチャンク
        """
        import time
        
        if chunk_size is None:
            chunk_size = self.CHUNK_SIZE
        
        start_time = time.time()
        while time.time() - start_time < duration:
            yield self.generate_stress_chunk(chunk_size)

    def slow_generator(self, duration: float) -> Iterator[tuple[bytes, int]]:
        """
        低速テスト用データジェネレータ（1行/秒）
        
        Args:
            duration: テスト時間（秒）
        
        Yields:
            tuple[bytes, int]: (データ, 経過秒数)
        """
        import time
        
        start_time = time.time()
        last_send = -1
        
        while True:
            elapsed = time.time() - start_time
            if elapsed >= duration:
                break
            
            current_sec = int(elapsed)
            if current_sec > last_send:
                last_send = current_sec
                yield self.generate_slow_line(current_sec), current_sec
            
            time.sleep(0.1)

    def plotter_generator(self, duration: float) -> Iterator[tuple[bytes, float]]:
        """
        プロッタテスト用データジェネレータ（10Hz）
        
        Args:
            duration: テスト時間（秒）
        
        Yields:
            tuple[bytes, float]: (データ, 経過秒数)
        """
        import time
        
        # ヘッダー送信
        yield self.generate_plotter_header(), 0.0
        
        start_time = time.time()
        last_send = -0.1
        
        while True:
            elapsed = time.time() - start_time
            if elapsed >= duration:
                break
            
            # 100msごと（10Hz）
            if elapsed - last_send >= 0.1:
                last_send = elapsed
                yield self.generate_plotter_line(elapsed), elapsed
            
            time.sleep(0.01)

    def generate_plotter_fast_line(self, elapsed_sec: float) -> bytes:
        """
        高速プロッタテスト用CSV行生成 (1kHz対応)
        
        フォーマット: time,sin,cos,random\r\n
        - sin: 周期0.5秒、振幅100 (高速変化)
        - cos: 周期0.3秒、振幅80 (高速変化)
        - random: ランダムウォーク ±150
        """
        # Ch1: Sin波 (周期0.5秒、振幅100) - 高速変化
        ch1 = 100.0 * math.sin(2.0 * math.pi * elapsed_sec / 0.5)
        
        # Ch2: Cos波 (周期0.3秒、振幅80) - 高速変化
        ch2 = 80.0 * math.cos(2.0 * math.pi * elapsed_sec / 0.3)
        
        # Ch3: ランダムウォーク
        self.random_walk += (random.random() * 2 - 1) * 2.0
        self.random_walk = max(-150.0, min(150.0, self.random_walk))
        
        line = f"{elapsed_sec:.3f},{ch1:.2f},{ch2:.2f},{self.random_walk:.2f}\r\n"
        data = line.encode('utf-8')
        self.update_checksum(data)
        return data

    def plotter_fast_generator(self, duration: float) -> Iterator[tuple[bytes, float]]:
        """
        高速プロッタテスト用データジェネレータ (1kHz)
        
        Args:
            duration: テスト時間（秒）
        
        Yields:
            tuple[bytes, float]: (データ, 経過秒数)
        """
        import time
        
        # ヘッダー送信
        yield self.generate_plotter_header(), 0.0
        
        start_time = time.time()
        last_send = -0.001
        
        while True:
            elapsed = time.time() - start_time
            if elapsed >= duration:
                break
            
            # 1msごと（1kHz）
            if elapsed - last_send >= 0.001:
                last_send = elapsed
                yield self.generate_plotter_fast_line(elapsed), elapsed
            
            time.sleep(0.0001)  # 0.1ms sleep for timing precision
