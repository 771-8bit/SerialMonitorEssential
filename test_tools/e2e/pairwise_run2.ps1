# Pairwise covering-array E2E runner - round 2 (full 12-factor model)
# Generated rows: see pairwise_gen2.py. Constraints are handled as execution
# don't-cares (inert factors are skipped); docs/24 §5.6 documents the caveat.
param(
  [Parameter(Mandatory = $true)][string]$LogPath,
  [string]$TestToolsDir = "C:\Users\kazuki\Documents\SerialMonitorEssential\test_tools"
)

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$auto = [System.Windows.Automation.AutomationElement]

$sig = @'
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
[DllImport("user32.dll")] public static extern void mouse_event(int flags, int dx, int dy, int b, int info);
[DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
'@
$win32 = Add-Type -MemberDefinition $sig -Name Win32P2 -Namespace WP2 -PassThru

function Find-Win([string]$title) {
  $cond = New-Object System.Windows.Automation.PropertyCondition($auto::NameProperty, $title)
  return $auto::RootElement.FindFirst([System.Windows.Automation.TreeScope]::Children, $cond)
}
function Activate($win) {
  # SetForegroundWindow can silently fail (foreground lock); verify and retry,
  # otherwise mouse/wheel events land on whatever window IS foreground.
  $h = [IntPtr]$win.Current.NativeWindowHandle
  for ($t = 0; $t -lt 5; $t++) {
    [void]$win32::SetForegroundWindow($h)
    Start-Sleep -Milliseconds 350
    if ($win32::GetForegroundWindow() -eq $h) { return }
  }
  Write-Host "WARN: could not bring window to foreground"
}
function Get-Buttons($win) {
  $cond = New-Object System.Windows.Automation.PropertyCondition($auto::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)
  return $win.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond)
}
function Click-Btn($win, [string]$name) {
  foreach ($b in (Get-Buttons $win)) {
    if ($b.Current.Name -eq $name -or $b.Current.Name -like "*$name*") {
      $b.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
      return $true
    }
  }
  return $false
}
function Get-Checkbox($win, [string]$name) {
  $cond = New-Object System.Windows.Automation.PropertyCondition($auto::ControlTypeProperty, [System.Windows.Automation.ControlType]::CheckBox)
  foreach ($cb in $win.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond)) {
    if ($name -eq "" -and [string]::IsNullOrEmpty($cb.Current.Name)) { return $cb }
    if ($name -ne "" -and $cb.Current.Name -like "*$name*") { return $cb }
  }
  return $null
}
function Ensure-Toggle($win, [string]$name, [bool]$desired) {
  $cb = Get-Checkbox $win $name
  if ($null -eq $cb) { throw "checkbox '$name' not found" }
  for ($i = 0; $i -lt 3; $i++) {
    $tp = $cb.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
    if (($tp.Current.ToggleState -eq [System.Windows.Automation.ToggleState]::On) -eq $desired) { return }
    $tp.Toggle(); Start-Sleep -Milliseconds 400
  }
  $tp = $cb.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
  if (($tp.Current.ToggleState -eq [System.Windows.Automation.ToggleState]::On) -ne $desired) {
    throw "checkbox '$name' did not reach $desired"
  }
}
function Select-Combo($win, [string]$valuePattern, [string]$itemPattern) {
  $cond = New-Object System.Windows.Automation.PropertyCondition($auto::ControlTypeProperty, [System.Windows.Automation.ControlType]::ComboBox)
  $combo = $null
  foreach ($c in $win.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond)) {
    try { $v = $c.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value } catch { $v = "" }
    if ($v -match $valuePattern) { $combo = $c; break }
  }
  if ($null -eq $combo) { throw "combo '$valuePattern' not found" }
  Activate $win
  $exp = $combo.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern)
  $liCond = New-Object System.Windows.Automation.PropertyCondition($auto::ControlTypeProperty, [System.Windows.Automation.ControlType]::ListItem)
  $target = $null
  for ($a = 0; $a -lt 4 -and $null -eq $target; $a++) {
    $exp.Expand(); Start-Sleep -Milliseconds (600 + 300 * $a)
    $items = $combo.FindAll([System.Windows.Automation.TreeScope]::Descendants, $liCond)
    if ($items.Count -eq 0) { $items = $auto::RootElement.FindAll([System.Windows.Automation.TreeScope]::Descendants, $liCond) }
    foreach ($it in $items) { if ($it.Current.Name -match $itemPattern) { $target = $it } }
  }
  if ($null -eq $target) { try { $exp.Collapse() } catch {}; throw "item '$itemPattern' not found" }
  $target.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
  Start-Sleep -Milliseconds 300
  try { $exp.Collapse() } catch {}
}
function Get-Texts($win) {
  $out = @()
  foreach ($e in $win.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)) {
    if ($e.Current.Name) { $out += $e.Current.Name }
  }
  return $out
}
function Wheel-On($win) {
  Activate $win
  $r = $win.Current.BoundingRectangle
  $cx = [int]($r.X + $r.Width / 2); $cy = [int]($r.Y + $r.Height * 0.4)
  [void]$win32::SetCursorPos($cx, $cy)
  Start-Sleep -Milliseconds 200
  $win32::mouse_event(0x0800, 0, 0, -120, 0)
  Start-Sleep -Milliseconds 500
}
function Click-Text($win, [string]$pattern) {
  Activate $win
  foreach ($e in $win.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)) {
    if ($e.Current.Name -match $pattern) {
      $r = $e.Current.BoundingRectangle
      $cx = [int]($r.X + $r.Width / 2); $cy = [int]($r.Y + $r.Height / 2)
      [void]$win32::SetCursorPos($cx, $cy)
      Start-Sleep -Milliseconds 120
      $win32::mouse_event(0x02, 0, 0, 0, 0); $win32::mouse_event(0x04, 0, 0, 0, 0)
      Start-Sleep -Milliseconds 300
      return $true
    }
  }
  return $false
}

# --- stateful drivers ---
$script:senderProc = $null
function Ensure-Sender([bool]$on) {
  $alive = $script:senderProc -and -not $script:senderProc.HasExited
  if ($on -and -not $alive) {
    $script:senderProc = Start-Process -FilePath "python" `
      -ArgumentList "serial_test.py", "--source", "virtual", "--port", "COM15", "--mode", "plot:label", "--duration", "900" `
      -WorkingDirectory $TestToolsDir -WindowStyle Hidden -PassThru
    Start-Sleep -Seconds 2
  }
  if (-not $on -and $alive) {
    Stop-Process -Id $script:senderProc.Id -Force -Confirm:$false
    $script:senderProc = $null
    Start-Sleep -Milliseconds 500
  }
}

$script:hiddenSin = $false  # legend hidden-state tracker; resets when plotter reopens

# viewState driver: returns achieved state ('live'|'inspect'|'paused'|'skipped')
function Ensure-ViewState($plotter, [string]$target, [bool]$hasChart) {
  # normalize to LIVE first
  if (Click-Btn $plotter "Resume") { Start-Sleep -Milliseconds 500 }
  if (Click-Btn $plotter "LIVE") { Start-Sleep -Milliseconds 500 }
  switch ($target) {
    "live" { return "live" }
    "paused" { [void](Click-Btn $plotter "Pause"); Start-Sleep -Milliseconds 500; return "paused" }
    "inspect" {
      if (-not $hasChart) { return "skipped" }
      Wheel-On $plotter
      return "inspect"
    }
  }
  return "skipped"
}

# --- covering array (from pairwise_gen2.py; regenerate there when factors change) ---
$rows = @(
  @{connected="no"; stream="off"; viewMode="hex"; lineWrap="on"; timestamp="on"; separator="Tab"; autoScroll="on"; plotterOpen="yes"; aggMode="LTTB"; viewState="paused"; windowSec="300"; hiddenCh="sin"},
  @{connected="no"; stream="on"; viewMode="ascii"; lineWrap="off"; timestamp="off"; separator="Space"; autoScroll="off"; plotterOpen="no"; aggMode="Average"; viewState="live"; windowSec="10"; hiddenCh="none"},
  @{connected="no"; stream="on"; viewMode="hex"; lineWrap="on"; timestamp="off"; separator="Comma"; autoScroll="on"; plotterOpen="no"; aggMode="LTTB"; viewState="inspect"; windowSec="1"; hiddenCh="none"},
  @{connected="yes"; stream="off"; viewMode="ascii"; lineWrap="on"; timestamp="on"; separator="Space"; autoScroll="off"; plotterOpen="yes"; aggMode="Average"; viewState="inspect"; windowSec="1"; hiddenCh="sin"},
  @{connected="yes"; stream="on"; viewMode="hex"; lineWrap="off"; timestamp="on"; separator="Tab"; autoScroll="on"; plotterOpen="no"; aggMode="Average"; viewState="paused"; windowSec="10"; hiddenCh="sin"},
  @{connected="yes"; stream="off"; viewMode="hex"; lineWrap="off"; timestamp="off"; separator="Comma"; autoScroll="off"; plotterOpen="yes"; aggMode="LTTB"; viewState="live"; windowSec="300"; hiddenCh="none"},
  @{connected="yes"; stream="off"; viewMode="hex"; lineWrap="on"; timestamp="on"; separator="Space"; autoScroll="on"; plotterOpen="no"; aggMode="LTTB"; viewState="live"; windowSec="10"; hiddenCh="sin"},
  @{connected="yes"; stream="on"; viewMode="hex"; lineWrap="on"; timestamp="on"; separator="Comma"; autoScroll="on"; plotterOpen="yes"; aggMode="Average"; viewState="paused"; windowSec="300"; hiddenCh="none"},
  @{connected="yes"; stream="on"; viewMode="ascii"; lineWrap="on"; timestamp="on"; separator="Comma"; autoScroll="on"; plotterOpen="yes"; aggMode="LTTB"; viewState="paused"; windowSec="10"; hiddenCh="sin"},
  @{connected="yes"; stream="on"; viewMode="ascii"; lineWrap="on"; timestamp="off"; separator="Tab"; autoScroll="off"; plotterOpen="yes"; aggMode="LTTB"; viewState="paused"; windowSec="1"; hiddenCh="sin"},
  @{connected="yes"; stream="on"; viewMode="ascii"; lineWrap="on"; timestamp="on"; separator="Space"; autoScroll="on"; plotterOpen="no"; aggMode="LTTB"; viewState="paused"; windowSec="300"; hiddenCh="sin"},
  @{connected="yes"; stream="on"; viewMode="hex"; lineWrap="off"; timestamp="on"; separator="Tab"; autoScroll="on"; plotterOpen="yes"; aggMode="LTTB"; viewState="inspect"; windowSec="300"; hiddenCh="none"},
  @{connected="yes"; stream="on"; viewMode="hex"; lineWrap="off"; timestamp="on"; separator="Tab"; autoScroll="on"; plotterOpen="yes"; aggMode="LTTB"; viewState="live"; windowSec="1"; hiddenCh="sin"},
  @{connected="yes"; stream="on"; viewMode="hex"; lineWrap="on"; timestamp="on"; separator="Tab"; autoScroll="on"; plotterOpen="yes"; aggMode="LTTB"; viewState="inspect"; windowSec="10"; hiddenCh="sin"}
)

$results = @()
$rowNum = 0
foreach ($row in $rows) {
  $rowNum++
  $failures = @()
  $logStart = (Get-Content $LogPath -ErrorAction SilentlyContinue | Measure-Object -Line).Lines
  try {
    $main = Find-Win "Serial Monitor Essential"
    if ($null -eq $main) { throw "main window missing" }

    # connected
    $btnNames = (Get-Buttons $main) | ForEach-Object { $_.Current.Name }
    $isConnected = $btnNames -contains "Disconnect"
    if ($row.connected -eq "yes" -and -not $isConnected) { [void](Click-Btn $main "Connect"); Start-Sleep -Seconds 1 }
    if ($row.connected -eq "no" -and $isConnected) { [void](Click-Btn $main "Disconnect"); Start-Sleep -Seconds 1 }

    # stream (sender on COM15; inert when disconnected but managed anyway)
    Ensure-Sender ($row.stream -eq "on")

    # viewMode (unnamed checkbox: On = ascii)
    Ensure-Toggle $main "" ($row.viewMode -eq "ascii")
    Start-Sleep -Milliseconds 300

    # ascii-only toggles
    if ($row.viewMode -eq "ascii") {
      Ensure-Toggle $main "Line Wrap" ($row.lineWrap -eq "on")
      Ensure-Toggle $main "Timestamp" ($row.timestamp -eq "on")
      if ($row.timestamp -eq "on") {
        Select-Combo $main "^(Space|Comma|Tab)$" ("^" + $row.separator + "$")
      }
    }
    Ensure-Toggle $main "Auto Scroll" ($row.autoScroll -eq "on")

    # plotter open/close
    $plotter = Find-Win "Serial Plotter"
    if ($row.plotterOpen -eq "yes" -and $null -eq $plotter) {
      [void](Click-Btn $main "Plotter"); Start-Sleep -Seconds 3
      $plotter = Find-Win "Serial Plotter"
      $script:hiddenSin = $false
    }
    if ($row.plotterOpen -eq "no" -and $null -ne $plotter) {
      $plotter.GetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern).Close()
      Start-Sleep -Seconds 2
      $plotter = Find-Win "Serial Plotter"
      $script:hiddenSin = $false
    }

    if ($row.plotterOpen -eq "yes") {
      if ($null -eq $plotter) { throw "plotter window failed to open" }
      $texts = Get-Texts $plotter
      $hasChart = -not ($texts -match "No data yet")

      Select-Combo $plotter "LTTB|Average" ("^" + $row.aggMode + "$")
      Select-Combo $plotter "^\d+s$" ("^" + $row.windowSec + "s$")

      # hidden channel (needs a chart with the sin legend)
      if ($hasChart) {
        $wantHidden = ($row.hiddenCh -eq "sin")
        if ($wantHidden -ne $script:hiddenSin) {
          if (Click-Text $plotter "^sin$") { $script:hiddenSin = $wantHidden }
        }
      }

      $achieved = Ensure-ViewState $plotter $row.viewState $hasChart
      Start-Sleep -Milliseconds 800

      # oracles
      $texts = Get-Texts $plotter
      switch ($achieved) {
        "live" { if (-not ($texts -match "LIVE")) { $failures += "status not LIVE" } }
        "paused" { if (-not ($texts -match "Paused")) { $failures += "status not Paused" } }
        "inspect" { if (-not ($texts -match "Inspect")) { $failures += "status not Inspect (wheel may have missed chart)" } }
      }
    } else {
      if ($null -ne $plotter) { $failures += "plotter window failed to close" }
    }

    try { $null = Get-Process -Name "tauri-appserial-monitor-essential","serial-monitor-essential" -ErrorAction Stop } catch { $failures += "app process died" }

    $logNow = Get-Content $LogPath -ErrorAction SilentlyContinue
    if ($logNow) {
      $bad = ($logNow | Select-Object -Skip $logStart) | Where-Object { $_ -match "panicked|ERROR" }
      if ($bad) { $failures += ("log errors: " + (($bad | Select-Object -First 2) -join " / ")) }
    }
  } catch {
    $failures += ("EXCEPTION: " + $_.Exception.Message)
  }

  $status = if ($failures.Count -eq 0) { "PASS" } else { "FAIL: " + ($failures -join "; ") }
  $desc = ($row.GetEnumerator() | Sort-Object Name | ForEach-Object { "$($_.Name)=$($_.Value)" }) -join " "
  $results += "Row ${rowNum}: $status  [$desc]"
  Write-Host ("Row ${rowNum}: $status")
}

Ensure-Sender $false

Write-Host "===== SUMMARY ====="
$results | ForEach-Object { Write-Host $_ }
