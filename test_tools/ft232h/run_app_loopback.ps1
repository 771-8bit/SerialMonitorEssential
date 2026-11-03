# FT232H 自己ループバックでのアプリ受信試験を一括実行する（SYS-NF-101 / DEBT-9）
#
# やること:
#   1. アプリ起動 → UIA で AI Bridge ON・ボーレート選択・$Port へ接続
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
  [int]$SendSize = 4096
)

$ErrorActionPreference = "Stop"
$here = $PSScriptRoot
$root = Resolve-Path (Join-Path $here "..\..")
$ui = Join-Path $root "test_tools\e2e\ui.ps1"

function Fail([string]$message) {
  Write-Host "FAIL: $message"
  exit 1
}

if (-not $Exe) {
  foreach ($profile in @("release", "debug")) {
    $candidate = Join-Path $root "src-tauri\target\$profile\serial-monitor-essential.exe"
    if (Test-Path $candidate) { $Exe = $candidate; break }
  }
}
if (-not $Exe -or -not (Test-Path $Exe)) { Fail "app binary not found" }

$app = Start-Process -FilePath $Exe -PassThru
$found = $false
for ($i = 0; $i -lt 30; $i++) {
  Start-Sleep -Seconds 2
  powershell -File $ui -Action list 2>$null | Out-Null
  if ($LASTEXITCODE -eq 0) { $found = $true; break }
}
if (-not $found) { try { $app.Kill() } catch {}; Fail "app window did not appear" }
Start-Sleep -Seconds 4

function Invoke-Ui([string[]]$UiArgs, [string]$What) {
  for ($attempt = 0; $attempt -lt 3; $attempt++) {
    powershell -File $ui @UiArgs
    if ($LASTEXITCODE -eq 0) { return }
    Start-Sleep -Seconds 2
  }
  try { $app.Kill() } catch {}
  Fail "could not $What"
}

Invoke-Ui @("-Action", "toggle", "-Name", "AI Bridge", "-State", "On") "turn AI Bridge on"
Invoke-Ui @("-Action", "select-port", "-Name", $Port) "select $Port"
# ボーレートは独自ドロップダウン: トリガは Text '<rate>' + Text ' bps'、選択肢は ListItem '<rate>'。
# ' bps' をクリックして開き、ListItem に限定してクリックする（Text '<rate>' と名前が衝突するため）。
Invoke-Ui @("-Action", "click-text", "-Name", "^ bps$") "open baud dropdown"
Start-Sleep -Milliseconds 600
Invoke-Ui @("-Action", "click-text", "-Name", "^$Baud$", "-Path", "ListItem") "select baud $Baud"
Start-Sleep -Milliseconds 500
Invoke-Ui @("-Action", "click", "-Name", "Connect") "click Connect"
Start-Sleep -Seconds 2

python (Join-Path $here "app_loopback.py") --port $Port --baud $Baud --seconds $Seconds --send-size $SendSize
$result = $LASTEXITCODE

powershell -File $ui -Action close-window -WindowTitle "Serial Monitor Essential" 2>$null
Start-Sleep -Seconds 2
if (-not $app.HasExited) { try { $app.Kill() } catch {} }

if ($result -eq 0) { Write-Host "APP LOOPBACK: PASS" } else { Write-Host "APP LOOPBACK: FAILED" }
exit $result
