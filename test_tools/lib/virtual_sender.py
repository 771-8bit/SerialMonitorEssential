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
            mode: テストモード ('stress', 'slow', 'fast', 'plot:csv', 'plot:label', 'plot:csv:fast', 'plot:label:fast')
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
            elif mode == 'plot:csv':
                self._send_plotter(duration, on_progress)
            elif mode == 'fast':
                self._send_fast(duration, on_progress)
            elif mode == 'demo':
                self._send_demo(duration, on_progress)
            elif mode == 'plot:label':
                self._send_plotter_label(duration, on_progress)
            elif mode == 'plot:csv:fast':
                self._send_plotter_fast(duration, on_progress)
            elif mode == 'plot:label:fast':
                self._send_plotter_label_fast(duration, on_progress)
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

    def _send_demo(
        self,
        duration: float,
        on_progress: Optional[Callable[[str], None]] = None
    ):
        """デモモード送信"""
        start_time = time.time()
        
        while True:
            elapsed = time.time() - start_time
            if elapsed >= duration:
                break

            # 1. 一般的なHello World (CRLF)
            if on_progress: on_progress("Demo: Standard Hello World (CRLF)")
            for i in range(5):
                data = f"[{i+1}] Hello World standard CRLF mode\r\n".encode('utf-8')
                self.ser.write(data)
                self.generator.update_checksum(data)
                time.sleep(0.1)
                
            time.sleep(0.5)
            if time.time() - start_time >= duration: break

            # 2. 改行コードCRのみ
            if on_progress: on_progress("Demo: CR only line endings")
            for i in range(5):
                data = f"[{i+1}] Hello World CR only mode\r".encode('utf-8')
                self.ser.write(data)
                self.generator.update_checksum(data)
                time.sleep(0.1)

            time.sleep(0.5)
            if time.time() - start_time >= duration: break

            # 3. 改行コードLFのみ
            if on_progress: on_progress("Demo: LF only line endings")
            for i in range(5):
                data = f"[{i+1}] Hello World LF only mode\n".encode('utf-8')
                self.ser.write(data)
                self.generator.update_checksum(data)
                time.sleep(0.1)

            time.sleep(0.5)
            if time.time() - start_time >= duration: break

            # 4. Linewrapが効くようなデータ (Long line)
            if on_progress: on_progress("Demo: Long lines for wrapping test")
            long_line = "This is a very long line that should wrap around the screen. " * 5 + "\r\n"
            data = long_line.encode('utf-8')
            self.ser.write(data)
            self.generator.update_checksum(data)
            
            time.sleep(0.5)
            if time.time() - start_time >= duration: break

            # 5. 0x00~0xffまでの全データ
            if on_progress: on_progress("Demo: Binary 0x00-0xFF")
            all_bytes = bytes(range(256))
            self.ser.write(all_bytes)
            self.generator.update_checksum(all_bytes)
            # Add a newline for separation visually
            self.ser.write(b"\r\n")
            
            time.sleep(0.5)
            if time.time() - start_time >= duration: break

            # 6. 高速なデータ (Burst)
            if on_progress: on_progress("Demo: High speed burst (0.5s)")
            burst_start = time.time()
            while time.time() - burst_start < 0.5:
                data = self.generator.generate_stress_chunk()
                self.ser.write(data)
                time.sleep(0.001) # Minimal sleep to allow processing
            
            # End of cycle pause
            time.sleep(1.0)

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
        """プロッタテスト送信（10Hz CSV形式、数値+State統合）"""
        import math
        import random
        
        # ヘッダー送信（State列を追加）
        header = b"time,sin,cos,random,motor,pump\r\n"
        self.ser.write(header)
        self.generator.update_checksum(header)
        
        if on_progress:
            on_progress("Sent header: time,sin,cos,random,motor,pump")

        start_time = time.time()
        last_send = -0.1
        line_count = 0
        motor_state = "OFF"
        pump_state = "OFF"

        while True:
            elapsed = time.time() - start_time
            if elapsed >= duration:
                break

            # 100msごと（10Hz）全データを1行で送信
            if elapsed - last_send >= 0.1:
                last_send = elapsed
                
                # Toggle motor state every 3 seconds
                if int(elapsed) % 6 < 3:
                    motor_state = "ON"
                else:
                    motor_state = "OFF"
                
                # Toggle pump state every 5 seconds
                if int(elapsed) % 10 < 5:
                    pump_state = "ON"
                else:
                    pump_state = "OFF"
                
                # 数値データ生成
                sin_val = math.sin(elapsed * 2 * math.pi / 10) * 50 + 50
                cos_val = math.cos(elapsed * 2 * math.pi / 10) * 50 + 50
                rand_val = random.uniform(0, 100)
                
                # 全データを1行で送信（数値 + State）
                line = f"{elapsed:.2f},{sin_val:.2f},{cos_val:.2f},{rand_val:.2f},{motor_state},{pump_state}\r\n"
                data = line.encode('utf-8')
                self.ser.write(data)
                self.generator.update_checksum(data)
                line_count += 1

            # 10秒ごとに進捗表示
            if line_count % 100 == 0 and line_count > 0 and on_progress:
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

    def _send_plotter_label(
        self,
        duration: float,
        on_progress: Optional[Callable[[str], None]] = None
    ):
        """ラベル形式プロッタテスト送信（10Hz Labeled形式、数値+State統合）"""
        import math
        import random
        
        if on_progress:
            on_progress("Sending labeled format: time:X,sin:Y,cos:Z,random:W,motor:STATE,pump:STATE")

        start_time = time.time()
        last_send = -0.1
        line_count = 0
        motor_state = "OFF"
        pump_state = "OFF"

        while True:
            elapsed = time.time() - start_time
            if elapsed >= duration:
                break

            # 100msごと（10Hz）全データを1行で送信
            if elapsed - last_send >= 0.1:
                last_send = elapsed
                
                # Toggle motor state every 3 seconds
                if int(elapsed) % 6 < 3:
                    motor_state = "ON"
                else:
                    motor_state = "OFF"
                
                # Toggle pump state every 5 seconds
                if int(elapsed) % 10 < 5:
                    pump_state = "ON"
                else:
                    pump_state = "OFF"
                
                # 数値データ生成
                sin_val = math.sin(elapsed * 2 * math.pi / 10) * 50 + 50
                cos_val = math.cos(elapsed * 2 * math.pi / 10) * 50 + 50
                rand_val = random.uniform(0, 100)
                
                # 全データを1行で送信（数値 + State）
                line = f"time:{elapsed:.2f},sin:{sin_val:.2f},cos:{cos_val:.2f},random:{rand_val:.2f},motor:{motor_state},pump:{pump_state}\r\n"
                data = line.encode('utf-8')
                self.ser.write(data)
                self.generator.update_checksum(data)
                line_count += 1

            # 10秒ごとに進捗表示
            if line_count % 100 == 0 and line_count > 0 and on_progress:
                on_progress(f"[{int(elapsed):3d}s] Lines: {line_count}, Bytes: {self.generator.total_bytes:,}")

            time.sleep(0.01)

    def _send_plotter_fast(
        self,
        duration: float,
        on_progress: Optional[Callable[[str], None]] = None
    ):
        """高速プロッタテスト送信（1kHz CSV形式、数値+State統合）"""
        import math
        import random
        
        # ヘッダー送信（State列を追加）
        header = b"time,sin,cos,random,motor,pump\r\n"
        self.ser.write(header)
        self.generator.update_checksum(header)
        
        if on_progress:
            on_progress("Sent header: time,sin,cos,random,motor,pump (fast mode ~1kHz)")

        start_time = time.time()
        last_send = -0.001
        line_count = 0
        motor_state = "OFF"
        pump_state = "OFF"
        last_progress = 0

        while True:
            elapsed = time.time() - start_time
            if elapsed >= duration:
                break

            # 1msごと（1kHz）全データを1行で送信
            if elapsed - last_send >= 0.001:
                last_send = elapsed
                
                # Toggle motor state every 3 seconds
                if int(elapsed) % 6 < 3:
                    motor_state = "ON"
                else:
                    motor_state = "OFF"
                
                # Toggle pump state every 5 seconds
                if int(elapsed) % 10 < 5:
                    pump_state = "ON"
                else:
                    pump_state = "OFF"
                
                # 数値データ生成
                sin_val = math.sin(elapsed * 2 * math.pi / 10) * 50 + 50
                cos_val = math.cos(elapsed * 2 * math.pi / 10) * 50 + 50
                rand_val = random.uniform(0, 100)
                
                # 全データを1行で送信（数値 + State）
                line = f"{elapsed:.3f},{sin_val:.2f},{cos_val:.2f},{rand_val:.2f},{motor_state},{pump_state}\r\n"
                data = line.encode('utf-8')
                self.ser.write(data)
                self.generator.update_checksum(data)
                line_count += 1

            # 1秒ごとに進捗表示
            current_sec = int(elapsed)
            if current_sec > last_progress and on_progress:
                last_progress = current_sec
                on_progress(f"[{current_sec:3d}s] Lines: {line_count}, Bytes: {self.generator.total_bytes:,}")

    def _send_plotter_label_fast(
        self,
        duration: float,
        on_progress: Optional[Callable[[str], None]] = None
    ):
        """高速ラベル形式プロッタテスト送信（1kHz Labeled形式、数値+State統合）"""
        import math
        import random
        
        if on_progress:
            on_progress("Sending labeled format (fast mode ~1kHz): time:X,sin:Y,cos:Z,random:W,motor:STATE,pump:STATE")

        start_time = time.time()
        last_send = -0.001
        line_count = 0
        motor_state = "OFF"
        pump_state = "OFF"
        last_progress = 0

        while True:
            elapsed = time.time() - start_time
            if elapsed >= duration:
                break

            # 1msごと（1kHz）全データを1行で送信
            if elapsed - last_send >= 0.001:
                last_send = elapsed
                
                # Toggle motor state every 3 seconds
                if int(elapsed) % 6 < 3:
                    motor_state = "ON"
                else:
                    motor_state = "OFF"
                
                # Toggle pump state every 5 seconds
                if int(elapsed) % 10 < 5:
                    pump_state = "ON"
                else:
                    pump_state = "OFF"
                
                # 数値データ生成
                sin_val = math.sin(elapsed * 2 * math.pi / 10) * 50 + 50
                cos_val = math.cos(elapsed * 2 * math.pi / 10) * 50 + 50
                rand_val = random.uniform(0, 100)
                
                # 全データを1行で送信（数値 + State）
                line = f"time:{elapsed:.3f},sin:{sin_val:.2f},cos:{cos_val:.2f},random:{rand_val:.2f},motor:{motor_state},pump:{pump_state}\r\n"
                data = line.encode('utf-8')
                self.ser.write(data)
                self.generator.update_checksum(data)
                line_count += 1

            # 1秒ごとに進捗表示
            current_sec = int(elapsed)
            if current_sec > last_progress and on_progress:
                last_progress = current_sec
                on_progress(f"[{current_sec:3d}s] Lines: {line_count}, Bytes: {self.generator.total_bytes:,}")
