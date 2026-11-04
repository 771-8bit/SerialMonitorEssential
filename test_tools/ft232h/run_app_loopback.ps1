# FT232H 自己ループバックでのアプリ受信試験を一括実行する（SYS-NF-101 / DEBT-9）
#
# やること:
#   1. アプリ起動 → UIA で MCP Server ON・ボーレート選択・$Port へ接続
#   2. app_loopback.py（ブリッジ send で連続送信 → read_range で読み戻し SHA-256 比較、
#      digest の index_lag_bytes を記録）
#   3. 後片付け（アプリ終了）
#
# 前提: FT232H の D0(TX)-D1(RX) をジャンパ、pyserial 不要（ブリッジ経由のみ）、
#       release ビルド済み（npm run tauri build -- --no-bundle）
#
# 使い方:
#   .\run_app_loopback.ps1                                   # COM5, 12000000, 60 秒, 4096 B/send
#   .\run_app_loopback.ps1 -Baud 3000000 -Seconds 30 -SendSize 1024

param(
  [string]$Exe = "",
  [string]$Port = "COM5",
  [ValidateSet("9600", "19200", "38400", "57600", "115200", "230400", "460800", "921600",
               "1000000", "2000000", "3000000", "12000000")]
  [string]$Baud = "12000000",
  [double]$Seconds = 60,
  [int]$SendSize = 4096,
  # "Hardware" には D2(RTS)-D3(CTS) のジャンパが必要
  [ValidateSet("None", "Hardware")]
  [string]$FlowControl = "None",
  # アプリの stderr（env_logger。5 秒ごとの [Worker] Status に受信停止の最大時間が出る）
  [string]$LogPath = ""
)

# ネイティブコマンド（子 powershell）の stderr を PS 5.1 は ErrorRecord 化し、"Stop" だと
# 呼び出し側のリダイレクト次第でスクリプトが後片付けなしに中断する。失敗は終了コードで扱う。
$ErrorActionPreference = "Continue"
$here = $PSScriptRoot
$root = Resolve-Path (Join-Path $here "..\..")
$ui = Join-Path $root "test_tools\e2e\ui.ps1"

function Fail([string]$message) {
  Write-Host "FAIL: $message"
  exit 1
}

# 同名ウィンドウが残っていると UIA が古い方を操作する（前回の異常終了の残骸など）
# Get-Process -Name はこの名前で誤判定する癖がある（docs/24 §6.5）ので CIM で数える
if (Get-CimInstance Win32_Process -Filter "Name='serial-monitor-essential.exe'") {
  Fail "another serial-monitor-essential is running; close it first"
}

if (-not $Exe) {
  foreach ($profile in @("release", "debug")) {
    $candidate = Join-Path $root "src-tauri\target\$profile\serial-monitor-essential.exe"
    if (Test-Path $candidate) { $Exe = $candidate; break }
  }
}
if (-not $Exe -or -not (Test-Path $Exe)) { Fail "app binary not found" }

if ($LogPath) {
  $app = Start-Process -FilePath $Exe -PassThru -RedirectStandardError $LogPath -RedirectStandardOutput "$LogPath.out"
} else {
  $app = Start-Process -FilePath $Exe -PassThru
}
function Stop-App {
  powershell -File $ui -Action close-window -WindowTitle "Serial Monitor Essential" 2>$null | Out-Null
  Start-Sleep -Seconds 2
  if (-not $app.HasExited) { try { $app.Kill() } catch {} }
}
$found = $false
for ($i = 0; $i -lt 30; $i++) {
  Start-Sleep -Seconds 2
  powershell -File $ui -Action list 2>$null | Out-Null
  if ($LASTEXITCODE -eq 0) { $found = $true; break }
}
if (-not $found) { Stop-App; Fail "app window did not appear" }
Start-Sleep -Seconds 4

function Invoke-Ui([string[]]$UiArgs, [string]$What) {
  for ($attempt = 0; $attempt -lt 3; $attempt++) {
    powershell -File $ui @UiArgs
    if ($LASTEXITCODE -eq 0) { return }
    Start-Sleep -Seconds 2
  }
  Stop-App
  Fail "could not $What"
}

Invoke-Ui @("-Action", "toggle", "-Name", "MCP Server", "-State", "On") "turn MCP Server on"
Invoke-Ui @("-Action", "select-port", "-Name", $Port) "select $Port"
# ボーレートは独自ドロップダウン: トリガは Text '<rate>' + Text ' bps'、選択肢は ListItem '<rate>'。
# ' bps' をクリックして開き、ListItem に限定してクリックする（Text '<rate>' と名前が衝突するため）。
# 開く→選ぶを 1 組でリトライする（ウィンドウ移動直後などに開くクリックが空振りすることがある）
$baudOk = $false
for ($attempt = 0; $attempt -lt 3 -and -not $baudOk; $attempt++) {
  powershell -File $ui -Action click-text -Name "^ bps$"
  Start-Sleep -Milliseconds (600 + 400 * $attempt)
  powershell -File $ui -Action click-text -Name "^$Baud$" -Path ListItem
  $baudOk = ($LASTEXITCODE -eq 0)
}
if (-not $baudOk) { Stop-App; Fail "could not select baud $Baud" }
Start-Sleep -Milliseconds 500
if ($FlowControl -ne "None") {
  # Parity と Flow Control はどちらも値 "None"。select-combo は $Name を持つ方を選ぶ
  Invoke-Ui @("-Action", "select-combo", "-Path", "^None$", "-Name", "^$FlowControl$") "set flow control $FlowControl"
}
Invoke-Ui @("-Action", "click", "-Name", "Connect") "click Connect"
Start-Sleep -Seconds 2

try {
  python (Join-Path $here "app_loopback.py") --port $Port --baud $Baud --seconds $Seconds --send-size $SendSize
  $result = $LASTEXITCODE
} finally {
  Stop-App
}

if ($result -eq 0) { Write-Host "APP LOOPBACK: PASS" } else { Write-Host "APP LOOPBACK: FAILED" }
exit $result
