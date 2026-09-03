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
const uint32_t DATA_BAUD_RATE = 12000000;  // 12Mbps (データ送信)
const uint32_t CONTROL_BAUD_RATE = 115200; // 115200bps (制御)
const uint32_t CHUNK_SIZE = 1024;          // 1KB chunks
const uint32_t LED_BLINK_INTERVAL = 500;   // LED点滅間隔（ms）

// グローバル変数
uint8_t dataBuffer[CHUNK_SIZE];
uint32_t totalBytesSent = 0;
uint32_t testDuration = 0; // テスト時間（秒、0なら無限）
uint32_t testStartTime = 0;
bool testRunning = false;
bool slowTestMode = false;    // 低速テストモード
bool plotterTestMode = false; // プロッタテストモード
uint32_t slowTestCounter = 0; // 低速テスト用カウンタ
uint32_t lastSlowSend = 0;    // 低速テストの最後の送信時刻
float randomWalkValue = 0.0;  // ランダムウォーク値
SHA256 sha256;                // Crypto library

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
  SerialControl.println(
      "  START:<duration>  - Start stress test for <duration> seconds");
  SerialControl.println("  SLOW:<duration>   - Start slow test (1 line/sec) "
                        "for <duration> seconds");
  SerialControl.println("  PLOTTER:<duration> - Start plotter test (CSV data) "
                        "for <duration> seconds");
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
    if (plotterTestMode) {
      sendPlotterData();
    } else if (slowTestMode) {
      sendSlowData();
    } else {
      sendDataChunk();
    }

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
    startTest(duration, 0); // 高速テスト
  } else if (cmd.startsWith("SLOW:")) {
    int duration = cmd.substring(5).toInt();
    startTest(duration, 1); // 低速テスト
  } else if (cmd.startsWith("PLOTTER:")) {
    int duration = cmd.substring(8).toInt();
    startTest(duration, 2); // プロッタテスト
  } else if (cmd == "STOP") {
    stopTest();
  } else if (cmd == "STATUS") {
    printStatus();
  } else if (cmd == "IDENTIFY") {
    sendPortIdentification();
  } else {
    SerialControl.println("ERROR: Unknown command: " + cmd);
    SerialControl.println(
        "Available commands: START:<seconds>, "
        "SLOW:<seconds>, PLOTTER:<seconds>, STOP, STATUS, IDENTIFY");
  }
}

// testMode: 0=fast, 1=slow, 2=plotter
void startTest(uint32_t duration, int testMode) {
  testDuration = duration;
  testStartTime = millis();
  totalBytesSent = 0;
  testRunning = true;
  slowTestMode = (testMode == 1);
  plotterTestMode = (testMode == 2);
  slowTestCounter = 0;
  lastSlowSend = 0;

  // チェックサムリセット
  sha256.reset();

  // ランダムウォーク初期化（プロッタモード用）
  if (plotterTestMode) {
    randomWalkValue = 0.0;
  }

  // 制御ポートに通知
  const char *modeName;
  const char *startMsg;
  if (plotterTestMode) {
    modeName = "PLOTTER (CSV data, 10Hz)";
    startMsg = "PLOTTER_TEST_START";
  } else if (slowTestMode) {
    modeName = "SLOW (1 line/sec)";
    startMsg = "SLOW_TEST_START";
  } else {
    modeName = "FAST (12Mbps)";
    startMsg = "TEST_START";
  }
  SerialControl.println(startMsg);
  SerialControl.print("Mode: ");
  SerialControl.println(modeName);
  SerialControl.print("Duration: ");
  SerialControl.print(duration);
  SerialControl.println(duration == 0 ? " seconds (infinite)" : " seconds");
  SerialControl.flush();

  // プロッタモードの場合、データポートにヘッダー行を送信
  if (plotterTestMode) {
    const char *header = "time,sin,cos,random\r\n";
    size_t headerLen = strlen(header);
    Serial.write((uint8_t *)header, headerLen);
    totalBytesSent += headerLen;
    sha256.update((uint8_t *)header, headerLen);
  }
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
    if (hash[i] < 0x10)
      SerialControl.print('0');
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

void sendSlowData() {
  // 1秒ごとに1行送信
  uint32_t now = millis();
  if (now - lastSlowSend < 1000) {
    return; // まだ1秒経っていない
  }
  lastSlowSend = now;

  // 送信する文字列を作成
  char line[64];
  uint32_t elapsed = (millis() - testStartTime) / 1000;
  int len =
      snprintf(line, sizeof(line), "[%04lu] Hello from Pico! Counter=%lu\r\n",
               elapsed, slowTestCounter);

  // データ送信（Serialへ）
  size_t written = Serial.write((uint8_t *)line, len);

  if (written > 0) {
    totalBytesSent += written;
    slowTestCounter++;

    // チェックサム更新
    sha256.update((uint8_t *)line, written);

    // 制御ポートにも進捗表示
    SerialControl.print("Sent line ");
    SerialControl.print(slowTestCounter);
    SerialControl.print(": ");
    SerialControl.print(written);
    SerialControl.println(" bytes");
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

// プロッタテスト用データ送信（100msごとにCSV行送信）
void sendPlotterData() {
  static uint32_t lastPlotterSend = 0;
  uint32_t now = millis();

  // 100msごと（10Hz）
  if (now - lastPlotterSend < 100) {
    return;
  }
  lastPlotterSend = now;

  // 経過時間（秒、小数点以下2桁）
  float elapsed = (now - testStartTime) / 1000.0;

  // 各チャンネルのデータ生成
  // Ch1: Sin波 (周期2秒、振幅100)
  float ch1 = 100.0 * sin(2.0 * PI * elapsed / 2.0);

  // Ch2: Cos波 (周期3秒、振幅80)
  float ch2 = 80.0 * cos(2.0 * PI * elapsed / 3.0);

  // Ch3: ランダムウォーク (±5の範囲で変動)
  randomWalkValue += (random(-100, 101) / 100.0) * 5.0;
  // -150～150の範囲にクランプ
  if (randomWalkValue > 150.0)
    randomWalkValue = 150.0;
  if (randomWalkValue < -150.0)
    randomWalkValue = -150.0;

  // CSVフォーマットで送信: timestamp,ch1,ch2,ch3
  char line[80];
  int len = snprintf(line, sizeof(line), "%.2f,%.2f,%.2f,%.2f\r\n", elapsed,
                     ch1, ch2, randomWalkValue);

  // データ送信（Serialへ）
  size_t written = Serial.write((uint8_t *)line, len);

  if (written > 0) {
    totalBytesSent += written;
    slowTestCounter++; // カウンタとして再利用

    // チェックサム更新
    sha256.update((uint8_t *)line, written);

    // 10秒ごとに制御ポートに進捗表示
    if (slowTestCounter % 100 == 0) {
      SerialControl.print("Plotter: ");
      SerialControl.print(slowTestCounter);
      SerialControl.print(" lines, ");
      SerialControl.print(totalBytesSent);
      SerialControl.println(" bytes");
    }
  }
}
