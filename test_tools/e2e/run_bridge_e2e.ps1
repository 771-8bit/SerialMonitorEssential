# AI Bridge E2E の一括実行（docs/23 GAP-31 のスクリプト化）
#
# やること:
#   1. com0com ペア（$PortBot ⇔ $PortApp）の存在確認
#   2. アプリ起動 → UIA で AI Bridge ON・$PortApp へ接続
#   3. $PortBot に pong_bot.py を配置
#   4. mcp_bridge_live.py（内蔵 MCP アダプタ経由の実往復 + ping/cancel 検証）
#   5. 後片付け（ボット停止・アプリ終了）
#
# 前提: com0com ペア作成済み（README.md 参照）、pyserial インストール済み、
#       バイナリのビルド済み（cd src-tauri; cargo build --release）
#
# 使い方:
#   .\run_bridge_e2e.ps1                # release バイナリを自動検出
#   .\run_bridge_e2e.ps1 -Exe <path> -PortApp COM16 -PortBot COM15

param(
  [string]$Exe = "",
  [string]$PortApp = "COM16",
  [string]$PortBot = "COM15"
)

$ErrorActionPreference = "Stop"
$here = $PSScriptRoot
$root = Resolve-Path (Join-Path $here "..\..")
$ui = Join-Path $here "ui.ps1"

function Fail([string]$message) {
  Write-Host "FAIL: $message"
  exit 1
}

# --- 1. 前提確認 -------------------------------------------------------------

if (-not $Exe) {
  foreach ($profile in @("release", "debug")) {
    $candidate = Join-Path $root "src-tauri\target\$profile\serial-monitor-essential.exe"
    if (Test-Path $candidate) { $Exe = $candidate; break }
  }
}
if (-not $Exe -or -not (Test-Path $Exe)) {
  Fail "app binary not found. Build first: cd src-tauri; cargo build --release"
}

$serialMap = Get-ItemProperty "HKLM:\HARDWARE\DEVICEMAP\SERIALCOMM" -ErrorAction SilentlyContinue
$ports = $serialMap.PSObject.Properties | Where-Object { $_.Name -notmatch '^PS' } | ForEach-Object { $_.Value }
foreach ($required in @($PortApp, $PortBot)) {
  if ($ports -notcontains $required) {
    Fail "$required not found. Create the com0com pair first (see README.md)."
  }
}
Write-Host "OK  com0com pair present ($PortBot <-> $PortApp), exe: $Exe"

# --- 2. アプリ起動 + UIA セットアップ ----------------------------------------

$app = Start-Process -FilePath $Exe -PassThru
$found = $false
for ($i = 0; $i -lt 30; $i++) {
  Start-Sleep -Seconds 2
  powershell -File $ui -Action list 2>$null | Out-Null
  if ($LASTEXITCODE -eq 0) { $found = $true; break }
}
if (-not $found) { try { $app.Kill() } catch {}; Fail "app window did not appear" }
# ウィンドウ出現 != React マウント完了。少し待ってから操作する。
Start-Sleep -Seconds 4

# UIA 操作は前面化のフレークで空振りすることがあるため 1 手ごとにリトライする
function Invoke-Ui([string[]]$UiArgs, [string]$What) {
  for ($attempt = 0; $attempt -lt 3; $attempt++) {
    powershell -File $ui @UiArgs
    if ($LASTEXITCODE -eq 0) { return }
    Start-Sleep -Seconds 2
  }
  try { $app.Kill() } catch {}
  Fail "could not $What"
}

Invoke-Ui @("-Action", "toggle", "-Name", "AI Bridge") "toggle AI Bridge"
Invoke-Ui @("-Action", "select-port", "-Name", $PortApp) "select $PortApp"
Invoke-Ui @("-Action", "click", "-Name", "Connect") "click Connect"

# --- 3. pong_bot -------------------------------------------------------------

$bot = Start-Process -FilePath "python" `
  -ArgumentList @((Join-Path $here "pong_bot.py"), "--port", $PortBot) `
  -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 2
if ($bot.HasExited) {
  try { $app.Kill() } catch {}
  Fail "pong_bot exited immediately (is $PortBot free? is pyserial installed?)"
}

# --- 4. ライブ検証 -----------------------------------------------------------

python (Join-Path $here "mcp_bridge_live.py") --exe $Exe --expect-port $PortApp
$result = $LASTEXITCODE

# --- 5. 後片付け -------------------------------------------------------------

try { Stop-Process -Id $bot.Id -Force -Confirm:$false } catch {}
powershell -File $ui -Action close-window -WindowTitle "Serial Monitor Essential" 2>$null
Start-Sleep -Seconds 2
if (-not $app.HasExited) { try { $app.Kill() } catch {} }

if ($result -eq 0) { Write-Host "BRIDGE E2E: ALL PASS" } else { Write-Host "BRIDGE E2E: FAILED" }
exit $result
