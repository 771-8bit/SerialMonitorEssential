#!/usr/bin/env python3
"""
シリアルデータ受信モジュール

仮想COMポートからデータを受信し、検証します。
SendPanel機能のテストに使用。
"""

import hashlib
import serial
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional


@dataclass
class ReceiveResult:
    """受信結果"""
    success: bool
    total_bytes: int
    checksum: str
    data: bytes
    error: Optional[str] = None


class SerialReceiver:
    """シリアルデータ受信クラス"""

    def __init__(self, port: str, baud: int = 115200, timeout: float = 1.0):
        self.port = port
        self.baud = baud
        self.timeout = timeout
        self.ser = None
        self.sha256 = hashlib.sha256()
        self.total_bytes = 0
        self.data_buffer = bytearray()

    def open(self) -> bool:
        """シリアルポートを開く"""
        try:
            self.ser = serial.Serial(self.port, self.baud, timeout=self.timeout)
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

    def get_checksum(self) -> str:
        """SHA256チェックサムを取得"""
        return self.sha256.hexdigest().upper()

    def receive(
        self,
        total_timeout: float = 30.0,
        idle_timeout: float = 3.0,
        on_progress: Optional[Callable[[str], None]] = None,
        on_data: Optional[Callable[[bytes], None]] = None
    ) -> ReceiveResult:
        """
        データを受信
        
        Args:
            total_timeout: 全体タイムアウト（秒）
            idle_timeout: アイドルタイムアウト（秒）- データ受信後、この時間データがなければ終了
            on_progress: 進捗コールバック
            on_data: データ受信コールバック
        
        Returns:
            ReceiveResult: 受信結果
        """
        start_time = time.time()
        last_receive_time = start_time
        last_progress_time = start_time

        try:
            while True:
                current_time = time.time()
                elapsed = current_time - start_time

                # 全体タイムアウトチェック
                if elapsed >= total_timeout:
                    if on_progress:
                        on_progress(f"Total timeout reached ({total_timeout}s)")
                    break

                # データ受信
                if self.ser.in_waiting > 0:
                    data = self.ser.read(min(self.ser.in_waiting, 4096))
                    if data:
                        self.total_bytes += len(data)
                        self.sha256.update(data)
                        self.data_buffer.extend(data)
                        last_receive_time = current_time
                        
                        if on_data:
                            on_data(data)
                else:
                    # アイドルタイムアウトチェック（データ受信後のみ）
                    if self.total_bytes > 0:
                        idle_time = current_time - last_receive_time
                        if idle_time >= idle_timeout:
                            if on_progress:
                                on_progress(f"Idle timeout reached ({idle_timeout}s)")
                            break

                # 1秒ごとに進捗表示
                if current_time - last_progress_time >= 1.0:
                    last_progress_time = current_time
                    if self.total_bytes > 0 and on_progress:
                        on_progress(f"[{int(elapsed):3d}s] Received: {self.total_bytes:,} bytes")

                time.sleep(0.01)

            return ReceiveResult(
                success=True,
                total_bytes=self.total_bytes,
                checksum=self.get_checksum(),
                data=bytes(self.data_buffer)
            )

        except KeyboardInterrupt:
            return ReceiveResult(
                success=False,
                total_bytes=self.total_bytes,
                checksum=self.get_checksum(),
                data=bytes(self.data_buffer),
                error="Interrupted by user"
            )
        except Exception as e:
            return ReceiveResult(
                success=False,
                total_bytes=self.total_bytes,
                checksum=self.get_checksum(),
                data=bytes(self.data_buffer),
                error=str(e)
            )

    def save_data(self, filepath: str):
        """受信データをファイルに保存"""
        Path(filepath).parent.mkdir(parents=True, exist_ok=True)
        with open(filepath, 'wb') as f:
            f.write(self.data_buffer)

    def verify(
        self,
        expected_bytes: Optional[int] = None,
        expected_checksum: Optional[str] = None
    ) -> tuple[bool, list[str]]:
        """
        受信データを検証
        
        Args:
            expected_bytes: 期待バイト数
            expected_checksum: 期待チェックサム
        
        Returns:
            tuple[bool, list[str]]: (全て合格か, エラーメッセージのリスト)
        """
        errors = []

        if expected_bytes is not None:
            if self.total_bytes != expected_bytes:
                diff = self.total_bytes - expected_bytes
                errors.append(f"Byte count mismatch: expected {expected_bytes:,}, got {self.total_bytes:,} (diff: {diff:+,})")

        if expected_checksum is not None:
            actual = self.get_checksum()
            if actual != expected_checksum.upper():
                errors.append(f"Checksum mismatch: expected {expected_checksum}, got {actual}")

        return len(errors) == 0, errors
