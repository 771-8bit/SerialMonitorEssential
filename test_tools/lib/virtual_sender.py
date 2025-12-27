#!/usr/bin/env python3
"""
仮想シリアルポート送信モジュール

仮想COMポートにテストデータを送信します。
DataGeneratorを使用してPicoと同じデータパターンを生成。
"""

import serial
import sys
import time
from dataclasses import dataclass
from typing import Callable, Optional

from .data_generator import DataGenerator


@dataclass
class VirtualSendResult:
    """送信結果"""
    success: bool
    total_bytes: int
    checksum: str
    mode: str
    duration: float
    error: Optional[str] = None


class VirtualSender:
    """仮想シリアルポートデータ送信クラス"""

    def __init__(self, port: str, baud: int = 115200):
        self.port = port
        self.baud = baud
        self.ser = None
        self.generator = DataGenerator()

    def open(self) -> bool:
        """シリアルポートを開く"""
        try:
            self.ser = serial.Serial(self.port, self.baud, timeout=1)
            time.sleep(0.5)  # ポート安定待ち
            return True
        except serial.SerialException as e:
            print(f"Error: Failed to open port {self.port}: {e}", file=sys.stderr)
            return False

    def close(self):
        """シリアルポートを閉じる"""
        if self.ser:
            self.ser.close()
            self.ser = None

    def send_data(
        self,
        mode: str,
        duration: float,
        on_progress: Optional[Callable[[str], None]] = None
    ) -> VirtualSendResult:
        """
        テストデータを送信
        
        Args:
            mode: テストモード ('stress', 'slow', 'fast', 'plotter')
            duration: テスト時間（秒）
            on_progress: 進捗コールバック関数
        
        Returns:
            VirtualSendResult: 送信結果
        """
        self.generator.reset()

        try:
            if mode == 'stress':
                self._send_stress(duration, on_progress)
            elif mode == 'slow':
                self._send_slow(duration, on_progress)
            elif mode == 'plotter':
                self._send_plotter(duration, on_progress)
            elif mode == 'fast':
                self._send_fast(duration, on_progress)
            else:
                return VirtualSendResult(
                    success=False,
                    total_bytes=0,
                    checksum="",
                    mode=mode,
                    duration=duration,
                    error=f"Unknown mode: {mode}"
                )

            return VirtualSendResult(
                success=True,
                total_bytes=self.generator.total_bytes,
                checksum=self.generator.get_checksum(),
                mode=mode,
                duration=duration
            )

        except KeyboardInterrupt:
            return VirtualSendResult(
                success=False,
                total_bytes=self.generator.total_bytes,
                checksum=self.generator.get_checksum(),
                mode=mode,
                duration=duration,
                error="Interrupted by user"
            )
        except Exception as e:
            return VirtualSendResult(
                success=False,
                total_bytes=self.generator.total_bytes,
                checksum=self.generator.get_checksum(),
                mode=mode,
                duration=duration,
                error=str(e)
            )

    def _send_stress(
        self,
        duration: float,
        on_progress: Optional[Callable[[str], None]] = None
    ):
        """高速ストレステスト送信"""
        start_time = time.time()
        last_progress = 0

        while True:
            elapsed = time.time() - start_time
            if elapsed >= duration:
                break

            data = self.generator.generate_stress_chunk()
            self.ser.write(data)

            # 1秒ごとに進捗表示
            current_sec = int(elapsed)
            if current_sec > last_progress and on_progress:
                last_progress = current_sec
                rate = self.generator.total_bytes / elapsed / 1024 / 1024
                on_progress(f"[{current_sec:3d}s] Sent: {self.generator.total_bytes:,} bytes ({rate:.2f} MB/s)")

    def _send_slow(
        self,
        duration: float,
        on_progress: Optional[Callable[[str], None]] = None
    ):
        """低速テスト送信（1行/秒）"""
        start_time = time.time()
        last_send = -1

        while True:
            elapsed = time.time() - start_time
            if elapsed >= duration:
                break

            current_sec = int(elapsed)
            if current_sec > last_send:
                last_send = current_sec
                data = self.generator.generate_slow_line(current_sec)
                self.ser.write(data)
                
                if on_progress:
                    on_progress(f"Sent line {self.generator.slow_counter}: {len(data)} bytes")

            time.sleep(0.1)

    def _send_plotter(
        self,
        duration: float,
        on_progress: Optional[Callable[[str], None]] = None
    ):
        """プロッタテスト送信（10Hz CSV）"""
        # ヘッダー送信
        header = self.generator.generate_plotter_header()
        self.ser.write(header)
        
        if on_progress:
            on_progress("Sent header: time,sin,cos,random")

        start_time = time.time()
        last_send = -0.1
        line_count = 0

        while True:
            elapsed = time.time() - start_time
            if elapsed >= duration:
                break

            # 100msごと（10Hz）
            if elapsed - last_send >= 0.1:
                last_send = elapsed
                data = self.generator.generate_plotter_line(elapsed)
                self.ser.write(data)
                line_count += 1

                # 10秒ごとに進捗表示
                if line_count % 100 == 0 and on_progress:
                    on_progress(f"[{int(elapsed):3d}s] Lines: {line_count}, Bytes: {self.generator.total_bytes:,}")

            time.sleep(0.01)

    def _send_fast(
        self,
        duration: float,
        on_progress: Optional[Callable[[str], None]] = None
    ):
        """高速テキスト送信（0.1msごと、slowと同じ内容）"""
        start_time = time.time()
        last_send = -0.0001
        line_count = 0

        while True:
            elapsed = time.time() - start_time
            if elapsed >= duration:
                break

            # 0.1msごと（10kHz）
            if elapsed - last_send >= 0.0001:
                last_send = elapsed
                data = self.generator.generate_slow_line(line_count)
                self.ser.write(data)
                line_count += 1

                # 1秒ごとに進捗表示
                if line_count % 100 == 0 and on_progress:
                    on_progress(f"[{int(elapsed):3d}s] Lines: {line_count}, Bytes: {self.generator.total_bytes:,}")
