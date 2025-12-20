/**
 * SerialMonitorEssential 高速シリアル送信テスト for Raspberry Pi Pico
 * 【デュアルCDCポート版】
 *
 * 12Mbpsでテストデータを送信し、SerialMonitorEssentialでの受信テストを行います。
 * 
 * CDC構成:
 * - Serial (CDC1): 12Mbps データ送信専用 → SerialMonitorEssentialが接続
 * - SerialControl (CDC2): 115200bps コマンド制御専用 → Pythonスクリプトが接続
 * 
 * 機能:
 * - コマンド制御モード（SerialControlから START:duration で開始）
 * - カウンタベースのデータ生成（検証可能）
 * - SHA256チェックサム計算と送信
 *
 * 配線: なし（USB接続のみ）
 */

#include <Adafruit_TinyUSB.h>
#include <Crypto.h>
#include <SHA256.h>

// 2つ目のCDCインターフェース（制御用）
Adafruit_USBD_CDC SerialControl;

// 設定
const uint32_t DATA_BAUD_RATE = 12000000;     // 12Mbps (データ送信)
const uint32_t CONTROL_BAUD_RATE = 115200;    // 115200bps (制御)
const uint32_t CHUNK_SIZE = 1024;             // 1KB chunks
const uint32_t LED_BLINK_INTERVAL = 500;      // LED点滅間隔（ms）

// グローバル変数
uint8_t dataBuffer[CHUNK_SIZE];
uint32_t totalBytesSent = 0;
uint32_t testDuration = 0;  // テスト時間（秒、0なら無限）
uint32_t testStartTime = 0;
bool testRunning = false;
SHA256 sha256;  // Crypto library

void setup() {
  // 組み込みLED初期化
  pinMode(LED_BUILTIN, OUTPUT);
  digitalWrite(LED_BUILTIN, LOW);

  // データ送信用CDC初期化（Serial）
  Serial.begin(DATA_BAUD_RATE);
  
  // 制御用CDC初期化（SerialControl）
  SerialControl.begin(CONTROL_BAUD_RATE);
  
  // 両方のUSB接続を待機（最大5秒）
  uint32_t start = millis();
  while ((!Serial || !SerialControl) && (millis() - start < 5000)) {
    delay(100);
  }
  
  delay(1000);
  
  // ポート識別情報を両方に送信
  sendPortIdentification();
  
  // 制御ポートに起動メッセージ
  SerialControl.println();
  SerialControl.println("=== SerialMonitorEssential Pico Test (Dual CDC) ===");
  SerialControl.println("Control Port (this port): 115200bps");
  SerialControl.println("Data Port (Serial): 12Mbps");
  SerialControl.println();
  SerialControl.println("Commands:");
  SerialControl.println("  START:<duration>  - Start test for <duration> seconds");
  SerialControl.println("  STOP              - Stop test");
  SerialControl.println("  STATUS            - Show current status");
  SerialControl.println("  IDENTIFY          - Show port identification info");
  SerialControl.println();
  SerialControl.println("Ready for commands.");
  SerialControl.println();
}

void loop() {
  // 制御コマンド受信チェック（SerialControlから）
  if (SerialControl.available()) {
    String command = SerialControl.readStringUntil('\n');
    command.trim();
    handleCommand(command);
  }
  
  // テスト実行中
  if (testRunning) {
    // テスト時間チェック
    if (testDuration > 0) {
      uint32_t elapsed = (millis() - testStartTime) / 1000;
      if (elapsed >= testDuration) {
        stopTest();
        return;
      }
    }
    
    // データ送信（Serialへ）
    sendDataChunk();
    
    // LED点滅
    static uint32_t lastBlink = 0;
    if (millis() - lastBlink > LED_BLINK_INTERVAL) {
      digitalWrite(LED_BUILTIN, !digitalRead(LED_BUILTIN));
      lastBlink = millis();
    }
  } else {
    // 待機中はLED消灯
    digitalWrite(LED_BUILTIN, LOW);
    delay(100);
  }
}

void sendPortIdentification() {
  // データポート（Serial）には識別情報を送信しない
  // （純粋なバイナリデータのみ）
  
  // 制御ポート（SerialControl）への識別情報
  SerialControl.println("PORT_TYPE: CONTROL");
  SerialControl.println("BAUD_RATE: 115200");
  SerialControl.println("PURPOSE: Command control from Python script");
  SerialControl.println("---");
  SerialControl.flush();
}

void handleCommand(String cmd) {
  if (cmd.startsWith("START:")) {
    int duration = cmd.substring(6).toInt();
    startTest(duration);
  } else if (cmd == "STOP") {
    stopTest();
  } else if (cmd == "STATUS") {
    printStatus();
  } else if (cmd == "IDENTIFY") {
    sendPortIdentification();
  } else {
    SerialControl.println("ERROR: Unknown command: " + cmd);
    SerialControl.println("Available commands: START:<seconds>, STOP, STATUS, IDENTIFY");
  }
}

void startTest(uint32_t duration) {
  testDuration = duration;
  testStartTime = millis();
  totalBytesSent = 0;
  testRunning = true;
  
  // チェックサムリセット
  sha256.reset();
  
  // 制御ポートに通知
  SerialControl.println("TEST_START");
  SerialControl.print("Duration: ");
  SerialControl.print(duration);
  SerialControl.println(duration == 0 ? " seconds (infinite)" : " seconds");
  SerialControl.flush();
  
  // データポートには何も送信しない（純粋なデータのみ）
}

void stopTest() {
  if (!testRunning) {
    SerialControl.println("ERROR: Test is not running");
    return;
  }
  
  testRunning = false;
  
  // チェックサム計算
  uint8_t hash[32];
  sha256.finalize(hash, 32);
  
  // 制御ポートに結果送信
  SerialControl.println();
  SerialControl.println("TEST_STOP");
  SerialControl.print("Total bytes: ");
  SerialControl.println(totalBytesSent);
  SerialControl.print("Checksum: ");
  for (int i = 0; i < 32; i++) {
    if (hash[i] < 0x10) SerialControl.print('0');
    SerialControl.print(hash[i], HEX);
  }
  SerialControl.println();
  SerialControl.flush();
  
  // データポートには何も送信しない（純粋なデータのみ）
}

void sendDataChunk() {
  // カウンタベースのテストデータ生成
  static uint32_t counter = 0;
  
  for (uint32_t i = 0; i < CHUNK_SIZE; i++) {
    dataBuffer[i] = (uint8_t)((counter + i) % 256);
  }
  
  // データ送信（Serialへ）
  size_t written = Serial.write(dataBuffer, CHUNK_SIZE);
  
  if (written > 0) {
    totalBytesSent += written;
    counter = (counter + written) % 256;
    
    // チェックサム更新
    sha256.update(dataBuffer, written);
  }
}

void printStatus() {
  SerialControl.println("STATUS:");
  SerialControl.print("  Running: ");
  SerialControl.println(testRunning ? "YES" : "NO");
  SerialControl.print("  Total bytes sent: ");
  SerialControl.println(totalBytesSent);
  
  if (testRunning && testDuration > 0) {
    uint32_t elapsed = (millis() - testStartTime) / 1000;
    SerialControl.print("  Elapsed: ");
    SerialControl.print(elapsed);
    SerialControl.print(" / ");
    SerialControl.print(testDuration);
    SerialControl.println(" seconds");
  }
}
