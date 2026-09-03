param(
  [Parameter(Mandatory=$true)][string]$Action,
  [string]$Name,
  [string]$WindowTitle = "Serial Monitor Essential",
  [string]$Path
)

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$auto = [System.Windows.Automation.AutomationElement]
$root = $auto::RootElement

function Find-Window([string]$title) {
  $cond = New-Object System.Windows.Automation.PropertyCondition($auto::NameProperty, $title)
  $w = $root.FindFirst([System.Windows.Automation.TreeScope]::Children, $cond)
  if ($null -eq $w) { throw "Window '$title' not found" }
  return $w
}

function Find-Buttons($win) {
  $cond = New-Object System.Windows.Automation.PropertyCondition($auto::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)
  return $win.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond)
}

function Activate-Window($win) {
  $hwnd = [IntPtr]$win.Current.NativeWindowHandle
  $sig = '[DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);'
  $t = Add-Type -MemberDefinition $sig -Name Win32SFW -Namespace W -PassThru
  [void]$t::SetForegroundWindow($hwnd)
  Start-Sleep -Milliseconds 300
}

switch ($Action) {
  "list" {
    $win = Find-Window $WindowTitle
    Write-Host "== Buttons =="
    foreach ($b in (Find-Buttons $win)) { Write-Host ("BTN: '" + $b.Current.Name + "'") }
    $cond = New-Object System.Windows.Automation.PropertyCondition($auto::ControlTypeProperty, [System.Windows.Automation.ControlType]::ComboBox)
    Write-Host "== ComboBoxes =="
    foreach ($c in $win.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond)) {
      $val = ""
      try { $vp = $c.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern); $val = $vp.Current.Value } catch {}
      Write-Host ("COMBO: name='" + $c.Current.Name + "' value='" + $val + "'")
    }
  }
  "select-port" {
    # Focus the port combobox (the one whose value mentions COM) and arrow
    # through options until the value contains $Name (e.g. COM16)
    $win = Find-Window $WindowTitle
    Activate-Window $win
    $cond = New-Object System.Windows.Automation.PropertyCondition($auto::ControlTypeProperty, [System.Windows.Automation.ControlType]::ComboBox)
    $combos = $win.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond)
    $portCombo = $null
    foreach ($c in $combos) {
      try {
        $vp = $c.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
        if ($vp.Current.Value -match "COM\d|CNC") { $portCombo = $c; break }
      } catch {}
    }
    if ($null -eq $portCombo) { throw "Port combobox not found" }
    $portCombo.SetFocus()
    Start-Sleep -Milliseconds 200
    $vp = $portCombo.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
    # Go to top of the list first
    [System.Windows.Forms.SendKeys]::SendWait("{HOME}")
    Start-Sleep -Milliseconds 250
    $tries = 0
    while ($tries -lt 20) {
      $cur = $vp.Current.Value
      if ($cur -match [regex]::Escape($Name)) { Write-Host ("SELECTED: " + $cur); exit 0 }
      [System.Windows.Forms.SendKeys]::SendWait("{DOWN}")
      Start-Sleep -Milliseconds 250
      $tries++
    }
    throw ("Could not select port containing '" + $Name + "'. Last value: " + $vp.Current.Value)
  }
  "click" {
    $win = Find-Window $WindowTitle
    $target = $null
    foreach ($b in (Find-Buttons $win)) {
      if ($b.Current.Name -eq $Name) { $target = $b; break }
    }
    if ($null -eq $target) {
      foreach ($b in (Find-Buttons $win)) {
        if ($b.Current.Name -like ("*" + $Name + "*")) { $target = $b; break }
      }
    }
    if ($null -eq $target) { throw "Button '$Name' not found" }
    $inv = $target.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
    $inv.Invoke()
    Write-Host ("CLICKED: " + $target.Current.Name)
  }
  "close-window" {
    $win = Find-Window $WindowTitle
    $wp = $win.GetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern)
    $wp.Close()
    Write-Host ("CLOSED: " + $WindowTitle)
  }
  "shot" {
    $win = Find-Window $WindowTitle
    Activate-Window $win
    Start-Sleep -Milliseconds 500
    $r = $win.Current.BoundingRectangle
    $bmp = New-Object System.Drawing.Bitmap([int]$r.Width, [int]$r.Height)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen([int]$r.X, [int]$r.Y, 0, 0, $bmp.Size)
    $bmp.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose(); $bmp.Dispose()
    Write-Host ("SAVED: " + $Path)
  }
  "toggle" {
    # Toggle a checkbox by (partial) name
    $win = Find-Window $WindowTitle
    $cond = New-Object System.Windows.Automation.PropertyCondition($auto::ControlTypeProperty, [System.Windows.Automation.ControlType]::CheckBox)
    $target = $null
    foreach ($cb in $win.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond)) {
      if ($cb.Current.Name -like ("*" + $Name + "*")) { $target = $cb; break }
    }
    if ($null -eq $target) { throw "Checkbox '$Name' not found" }
    $tp = $target.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
    $tp.Toggle()
    Write-Host ("TOGGLED: " + $target.Current.Name + " -> " + $tp.Current.ToggleState)
  }
  "select-combo" {
    # In window $WindowTitle, find combobox whose current value matches $Path
    # (regex) and select the dropdown item whose name matches $Name (regex)
    $win = Find-Window $WindowTitle
    $cond = New-Object System.Windows.Automation.PropertyCondition($auto::ControlTypeProperty, [System.Windows.Automation.ControlType]::ComboBox)
    $combo = $null
    foreach ($c in $win.FindAll([System.Windows.Automation.TreeScope]::Descendants, $cond)) {
      try { $v = $c.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value } catch { $v = "" }
      if ($v -match $Path) { $combo = $c; break }
    }
    if ($null -eq $combo) { throw "Combo with value matching '$Path' not found" }
    Activate-Window $win
    $exp = $combo.GetCurrentPattern([System.Windows.Automation.ExpandCollapsePattern]::Pattern)
    $liCond = New-Object System.Windows.Automation.PropertyCondition($auto::ControlTypeProperty, [System.Windows.Automation.ControlType]::ListItem)
    $target = $null
    for ($attempt = 0; $attempt -lt 4 -and $null -eq $target; $attempt++) {
      $exp.Expand(); Start-Sleep -Milliseconds (600 + 400 * $attempt)
      # Search the combo's own subtree first, then the whole desktop
      $items = $combo.FindAll([System.Windows.Automation.TreeScope]::Descendants, $liCond)
      if ($items.Count -eq 0) {
        $items = $auto::RootElement.FindAll([System.Windows.Automation.TreeScope]::Descendants, $liCond)
      }
      foreach ($it in $items) { if ($it.Current.Name -match $Name) { $target = $it } }
    }
    if ($null -eq $target) { try { $exp.Collapse() } catch {}; throw "Item '$Name' not found" }
    $target.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern).Select()
    Start-Sleep -Milliseconds 300
    try { $exp.Collapse() } catch {}
    Write-Host ("COMBO now: " + $combo.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value)
  }
  "click-text" {
    # Physically click the center of the first element whose name matches $Name
    $win = Find-Window $WindowTitle
    Activate-Window $win
    $all = $win.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
    $target = $null
    foreach ($e in $all) {
      if ($e.Current.Name -match $Name) { $target = $e; break }
    }
    if ($null -eq $target) { throw "Element matching '$Name' not found" }
    $r = $target.Current.BoundingRectangle
    $cx = [int]($r.X + $r.Width / 2); $cy = [int]($r.Y + $r.Height / 2)
    $sig = @'
[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
[DllImport("user32.dll")] public static extern void mouse_event(int flags, int dx, int dy, int b, int info);
'@
    $m = Add-Type -MemberDefinition $sig -Name Win32Mouse -Namespace W2 -PassThru
    [void]$m::SetCursorPos($cx, $cy)
    Start-Sleep -Milliseconds 120
    $m::mouse_event(0x02, 0, 0, 0, 0)  # left down
    $m::mouse_event(0x04, 0, 0, 0, 0)  # left up
    Write-Host ("CLICKED-AT: '" + $target.Current.Name + "' (" + $cx + "," + $cy + ")")
  }
  "wheel" {
    # Send a mouse wheel event at the center of the window's plot area
    # (center of the window, slightly above middle)
    $win = Find-Window $WindowTitle
    Activate-Window $win
    $r = $win.Current.BoundingRectangle
    $cx = [int]($r.X + $r.Width / 2); $cy = [int]($r.Y + $r.Height * 0.4)
    $sig = @'
[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
[DllImport("user32.dll")] public static extern void mouse_event(int flags, int dx, int dy, int b, int info);
'@
    $m = Add-Type -MemberDefinition $sig -Name Win32Wheel -Namespace W3 -PassThru
    [void]$m::SetCursorPos($cx, $cy)
    Start-Sleep -Milliseconds 200
    $m::mouse_event(0x0800, 0, 0, -120, 0)  # wheel down one notch
    Write-Host ("WHEEL at (" + $cx + "," + $cy + ")")
  }
  default { throw "Unknown action: $Action" }
}
