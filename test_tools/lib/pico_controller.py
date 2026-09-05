#!/usr/bin/env python3
"""
Raspberry Pi Pico コントローラーモジュール

Picoに対してテストコマンドを送信し、結果を受信します。
"""

import serial
import sys
import time
from dataclasses import dataclass
from typing import Callable, Optional


@dataclass
class PicoTestResult:
    """Picoテスト結果"""
    success: bool
    total_bytes: int
    checksum: str
    mode: str
    duration: int
    error: Optional[str] = None


class PicoController:
    """Raspberry Pi Pico テストコントローラー"""

    # テストモードとコマンドのマッピング
    MODE_COMMANDS = {
        'stress': 'START',
        'slow': 'SLOW',
        'plotter': 'PLOTTER',
    }

    def __init__(self, port: str, baud: int = 115200, timeout: float = 1.0):
        self.port = port
        self.baud = baud
        self.timeout = timeout
        self.ser = None

    def open(self) -> bool:
        """シリアルポートを開く"""
        try:
            self.ser = serial.Serial(self.port, self.baud, timeout=self.timeout)
            time.sleep(2)  # Picoのリセット待機
            # 既存の出力をクリア
            while self.ser.in_waiting:
                self.ser.read(self.ser.in_waiting)
            return True
        except serial.SerialException as e:
            print(f"Error: Failed to open port {self.port}: {e}", file=sys.stderr)
            return False

    def close(self):
        """シリアルポートを閉じる"""
        if self.ser:
            self.ser.close()
            self.ser = None

    def start_test(
        self,
        mode: str,
        duration: int,
        on_progress: Optional[Callable[[str], None]] = None
    ) -> PicoTestResult:
        """
        テストを開始し、完了まで待機
        
        Args:
            mode: テストモード ('stress', 'slow', 'plotter')
            duration: テスト時間（秒）
            on_progress: 進捗コールバック関数
        
        Returns:
            PicoTestResult: テスト結果
        """
        if mode not in self.MODE_COMMANDS:
            return PicoTestResult(
                success=False,
                total_bytes=0,
                checksum="",
                mode=mode,
                duration=duration,
                error=f"Unknown mode: {mode}"
            )

        command = f"{self.MODE_COMMANDS[mode]}:{duration}\n"
        
        if on_progress:
            on_progress(f"Sending command: {command.strip()}")
        
        self.ser.write(command.encode())
        self.ser.flush()

        # 結果待機
        return self._wait_for_result(mode, duration, on_progress)

    def _wait_for_result(
        self,
        mode: str,
        duration: int,
        on_progress: Optional[Callable[[str], None]] = None
    ) -> PicoTestResult:
        """テスト結果を待機"""
        start_time = time.time()
        test_started = False
        total_bytes = 0
        checksum = ""

        try:
            while True:
                if self.ser.in_waiting:
                    line = self.ser.readline().decode('utf-8', errors='ignore').strip()
                    
                    if not line:
                        continue
                    
                    if on_progress:
                        on_progress(f"[Pico] {line}")
                    
                    if line in ("TEST_START", "SLOW_TEST_START", "PLOTTER_TEST_START"):
                        test_started = True
                    
                    elif line == "TEST_STOP":
                        pass  # 続きで結果を受信
                    
                    elif line.startswith("Total bytes:"):
                        total_bytes = int(line.split(":")[1].strip())
                    
                    elif line.startswith("Checksum:"):
                        checksum = line.split(":")[1].strip()
                        break  # テスト完了
                
                # タイムアウトチェック
                if time.time() - start_time > duration + 30:
                    return PicoTestResult(
                        success=False,
                        total_bytes=total_bytes,
                        checksum=checksum,
                        mode=mode,
                        duration=duration,
                        error="Timeout waiting for test completion"
                    )
                
                time.sleep(0.1)

        except KeyboardInterrupt:
            self.stop_test()
            return PicoTestResult(
                success=False,
                total_bytes=total_bytes,
                checksum=checksum,
                mode=mode,
                duration=duration,
                error="Test interrupted by user"
            )

        if not test_started:
            return PicoTestResult(
                success=False,
                total_bytes=0,
                checksum="",
                mode=mode,
                duration=duration,
                error="Test did not start properly"
            )

        return PicoTestResult(
            success=True,
            total_bytes=total_bytes,
            checksum=checksum,
            mode=mode,
            duration=duration
        )

    def stop_test(self):
        """テストを中断"""
        if self.ser:
            self.ser.write(b"STOP\n")
            self.ser.flush()
