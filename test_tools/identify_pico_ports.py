#!/usr/bin/env python3
"""
COMポート識別ヘルパースクリプト

Raspberry Pi Picoの2つのCDCポートを識別し、
どちらが制御用でどちらがデータ用かを判定します。

使用方法:
    python identify_pico_ports.py
"""

import serial
import serial.tools.list_ports
import time


def find_pico_ports():
    """Picoの2つのCOMポートを検索"""
    ports = list(serial.tools.list_ports.comports())
    pico_ports = []
    
    print(f"Found {len(ports)} COM port(s) total:\n")
    
    for port in ports:
        print(f"Port: {port.device}")
        print(f"  Description: {port.description}")
        print(f"  HWID: {port.hwid}")
        print(f"  VID:PID: {port.vid:04X}:{port.pid:04X}" if port.vid else "  VID:PID: N/A")
        print(f"  Manufacturer: {port.manufacturer}")
        print(f"  Serial Number: {port.serial_number}")
        
        # Raspberry Pi Picoの識別
        # VID:PID = 2E8A:000A (Raspberry Pi 公式)
        # VID:PID = 239A:CAFE (Adafruit TinyUSB使用時)
        is_pico = False
        reason = []
        
        # Raspberry Pi 公式VID
        if port.vid == 0x2E8A:
            is_pico = True
            reason.append("VID matches (0x2E8A - Raspberry Pi)")
        
        # Adafruit TinyUSB VID (Picoで使用時)
        if port.vid == 0x239A:
            is_pico = True
            reason.append("VID matches (0x239A - Adafruit TinyUSB)")
        
        if '2E8A' in port.hwid.upper():
            is_pico = True
            reason.append("HWID contains '2E8A'")
        
        if '239A' in port.hwid.upper():
            is_pico = True
            reason.append("HWID contains '239A' (Adafruit TinyUSB)")
        
        if 'RASPBERRY' in port.description.upper() or 'PICO' in port.description.upper():
            is_pico = True
            reason.append("Description contains 'RASPBERRY' or 'PICO'")
        
        if is_pico:
            print(f"  ✓ Identified as Pico: {', '.join(reason)}")
            pico_ports.append(port)
        else:
            print(f"  ✗ Not identified as Pico")
        
        print()
    
    return pico_ports


def identify_port(port_name):
    """ポートに接続してIDENTIFYコマンドを送信し、応答を確認"""
    
    # まず115200bpsで試す（制御ポート用）
    port_type, response = try_identify_at_baud(port_name, 115200)
    if port_type != "UNKNOWN" and port_type != "ERROR":
        return port_type, response
    
    # 次に12Mbpsで試す（データポート用）
    port_type, response = try_identify_at_baud(port_name, 12000000)
    return port_type, response


def try_identify_at_baud(port_name, baud_rate):
    """指定されたボーレートでポートを識別"""
    try:
        ser = serial.Serial(port_name, baud_rate, timeout=2)
        time.sleep(0.5)
        
        # 既存のバッファをクリア
        ser.reset_input_buffer()
        
        # 起動時の識別メッセージを読み取る
        response = ""
        time.sleep(0.3)
        
        while ser.in_waiting:
            line = ser.readline().decode('utf-8', errors='ignore').strip()
            response += line + "\n"
            if len(response) > 500:  # 長すぎる場合は打ち切り
                break
        
        # IDENTIFYコマンドを送信
        ser.write(b"IDENTIFY\n")
        ser.flush()
        time.sleep(0.3)
        
        # 応答を読み取り
        while ser.in_waiting:
            line = ser.readline().decode('utf-8', errors='ignore').strip()
            response += line + "\n"
            if len(response) > 500:
                break
        
        ser.close()
        
        # PORT_TYPEを解析
        if "PORT_TYPE: CONTROL" in response:
            return "CONTROL", response
        elif "PORT_TYPE: DATA" in response:
            return "DATA", response
        else:
            return "UNKNOWN", response
    
    except Exception as e:
        return "ERROR", str(e)


def main():
    print("=== Raspberry Pi Pico Port Identifier ===\n")
    
    # Picoのポートを検索
    print("Searching for Raspberry Pi Pico ports...")
    pico_ports = find_pico_ports()
    
    if len(pico_ports) == 0:
        print("Error: No Raspberry Pi Pico found.")
        print("Please connect Pico and run again.")
        return 1
    
    print(f"Found {len(pico_ports)} Pico port(s):\n")
    
    control_port = None
    data_port = None
    unknown_ports = []
    
    # 各ポートを識別
    for port in pico_ports:
        print(f"Checking {port.device} ({port.description})...")
        port_type, response = identify_port(port.device)
        
        if port_type == "CONTROL":
            control_port = port.device
            print(f"  → CONTROL PORT (for Python script)")
        elif port_type == "DATA":
            data_port = port.device
            print(f"  → DATA PORT (for SerialMonitorEssential)")
        elif port_type == "UNKNOWN":
            unknown_ports.append(port)
            print(f"  → Unknown port type (possibly DATA port at 12Mbps)")
        else:
            print(f"  → Error: {response}")
        print()
    
    # 推定ロジック: CONTROLポートが見つかり、かつ同じシリアル番号の
    # Unknownポートがある場合、それをDATAポートと推定
    if control_port and not data_port and len(unknown_ports) > 0:
        # 同じシリアル番号のポートを探す
        control_serial = None
        for port in pico_ports:
            if port.device == control_port:
                control_serial = port.serial_number
                break
        
        if control_serial:
            for unknown_port in unknown_ports:
                if unknown_port.serial_number == control_serial:
                    data_port = unknown_port.device
                    print(f"[INFERRED] {data_port} is likely the DATA PORT")
                    print(f"  Reason: Same serial number as CONTROL port ({control_serial})")
                    print(f"  Note: 12Mbps connection test may not work with pyserial/Windows drivers")
                    print()
                    break
    
    # 結果サマリー
    print("=" * 50)
    print("SUMMARY:")
    print("=" * 50)
    
    if control_port:
        print(f"Control Port:  {control_port}")
        print("  → Use with: python pico_stress_test_controller.py --port {control_port}")
    else:
        print("Control Port: Not found")
    
    print()
    
    if data_port:
        print(f"Data Port:     {data_port}")
        print("  → Use with: SerialMonitorEssential (12Mbps)")
    else:
        print("Data Port: Not found")
    
    print()
    
    # 推奨アクション
    if control_port and data_port:
        print("✓ Both ports identified successfully!")
        print()
        print("Next steps:")
        print(f"1. Open SerialMonitorEssential and select: {data_port} at 12Mbps")
        print(f"2. Run: python pico_stress_test_controller.py --port {control_port} --duration 60")
    elif len(pico_ports) == 1:
        print("⚠ Only one port found. Pico may be using single CDC mode.")
        print("  Please upload the dual CDC firmware (pico_serial_tx_test.ino)")
    else:
        print("⚠ Could not identify all ports.")
        print("  Ports may be in use. Please close any programs using the Pico.")
    
    return 0


if __name__ == '__main__':
    import sys
    sys.exit(main())
