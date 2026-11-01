<#
.SYNOPSIS
  End-to-end test of the LiveTyper release build with real injected keys.

.DESCRIPTION
  The script starts target\release\livetyper.exe with LIVETYPER_TEST_AUDIO set to a WAV fixture,
  opens a new classic Notepad, holds the Talk Key (Ctrl+Space) with keybd_event, and reads the
  pasted text back from the Notepad "Edit" control.

  The script injects keys only while its own Notepad window is the foreground window. If the
  foreground window changes during a hold, the script pushes Escape (the app owns Escape during
  a Dictation) and releases the keys.

  Dot-source the script to use the functions one by one:
      . .\scripts\e2e-test.ps1 -NoRun
  Or run the default case list:
      .\scripts\e2e-test.ps1 -Wav tests\fixtures\tts-15s.wav

.PARAMETER Wav
  The 16 kHz mono WAV that plays in place of the microphone. The .txt file with the same name
  is the expected text.

.PARAMETER OutDir
  Folder for screenshots and the JSON result.

.PARAMETER Cases
  The test cases to run: idle, dictation, warm, tap, escape, image, quit.
#>
param(
    [string]$Wav = "tests\fixtures\tts-15s.wav",
    [string]$OutDir = "$env:TEMP\livetyper-e2e",
    [string[]]$Cases = @("idle", "dictation", "warm", "tap", "escape", "image", "quit"),
    [int]$Screenshots = 3,
    # Use another Talk Key when another app holds Ctrl+Space (PowerToys Peek uses it by default).
    # The script then writes it to settings.toml and restores the file byte for byte at the end.
    [ValidateSet("Ctrl+Space", "Ctrl+Shift+Space")][string]$TalkKey = "Ctrl+Space",
    [switch]$NoRun
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$Exe = Join-Path $Root "target\release\livetyper.exe"
$Log = Join-Path $env:LOCALAPPDATA "LiveTyper\livetyper.log"
New-Item -ItemType Directory -Force $OutDir | Out-Null

Add-Type -AssemblyName System.Windows.Forms, System.Drawing
if (-not ("E2E.Native" -as [type])) {
    Add-Type -Namespace E2E -Name Native -MemberDefinition @'
[DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
[DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
[DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
[DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
[DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
[DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool attach);
[DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
[DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindow(string cls, string title);
[DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr FindWindowEx(IntPtr parent, IntPtr after, string cls, string title);
[DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr SendMessage(IntPtr h, uint msg, IntPtr w, System.Text.StringBuilder l);
[DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr SendMessage(IntPtr h, uint msg, IntPtr w, string l);
[DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
[DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
[DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
[DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
[DllImport("user32.dll")] public static extern short GetAsyncKeyState(int vk);
[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
[DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
[DllImport("user32.dll")] public static extern bool GetCursorPos(out POINT p);
[DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(POINT p);
[DllImport("user32.dll")] public static extern void mouse_event(uint flags, int dx, int dy, uint data, UIntPtr extra);
[StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
[StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
'@
}
[E2E.Native]::SetProcessDPIAware() | Out-Null

$VK_CONTROL = 0x11; $VK_SHIFT = 0x10; $VK_SPACE = 0x20; $VK_ESCAPE = 0x1B
$Settings = Join-Path $env:APPDATA "LiveTyper\settings.toml"
$SettingsBackup = "$Settings.e2e-backup"
$script:Mods = @($VK_CONTROL)

function Set-TalkKey([string]$combo) {
    $script:Mods = if ($combo -eq "Ctrl+Shift+Space") { @($VK_CONTROL, $VK_SHIFT) } else { @($VK_CONTROL) }
    if ($combo -eq "Ctrl+Space") { return }
    if (-not (Test-Path $SettingsBackup)) { Copy-Item $Settings $SettingsBackup }
    $text = Get-Content $SettingsBackup -Raw
    if ($text -match "(?m)^talk_key\s*=") { $text = $text -replace "(?m)^talk_key\s*=.*$", "talk_key = `"$combo`"" }
    else { $text = "talk_key = `"$combo`"`r`n" + $text }
    [IO.File]::WriteAllText($Settings, $text)
}

function Restore-Settings {
    if (Test-Path $SettingsBackup) { Copy-Item $SettingsBackup $Settings -Force; Remove-Item $SettingsBackup }
}
$KEYUP = 2

function Key([int]$vk, [bool]$up) {
    [E2E.Native]::keybd_event([byte]$vk, 0, $(if ($up) { $KEYUP } else { 0 }), [UIntPtr]::Zero)
}

function Get-LogLineCount { if (Test-Path $Log) { @(Get-Content $Log).Count } else { 0 } }
function Get-LogSince([int]$from) { @(Get-Content $Log) | Select-Object -Skip $from }

function Stop-App {
    $tray = [E2E.Native]::FindWindow("LiveTyperTray", [NullString]::Value)
    if ($tray -ne [IntPtr]::Zero) { [E2E.Native]::PostMessage($tray, 0x10, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null }
    $p = Get-Process livetyper -ErrorAction SilentlyContinue
    if ($p) { $p | Wait-Process -Timeout 5 -ErrorAction SilentlyContinue }
    $alive = Get-Process livetyper -ErrorAction SilentlyContinue
    [pscustomobject]@{ TrayFound = ($tray -ne [IntPtr]::Zero); ExitedCleanly = -not $alive }
}

function Start-App([string]$wavPath) {
    Get-Process livetyper -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 500
    $env:LIVETYPER_TEST_AUDIO = (Resolve-Path $wavPath).Path
    $p = Start-Process $Exe -PassThru
    Remove-Item Env:LIVETYPER_TEST_AUDIO
    $p
}

function Get-AppMemory {
    $p = Get-Process livetyper -ErrorAction SilentlyContinue
    if (-not $p) { return $null }
    [pscustomobject]@{
        WorkingSetMB = [math]::Round($p.WorkingSet64 / 1MB, 1)
        PrivateMB    = [math]::Round($p.PrivateMemorySize64 / 1MB, 1)
        PeakWSMB     = [math]::Round($p.PeakWorkingSet64 / 1MB, 1)
    }
}

function Start-TestNotepad {
    $p = Start-Process notepad.exe -PassThru
    $p.WaitForInputIdle(5000) | Out-Null
    for ($i = 0; $i -lt 50 -and $p.MainWindowHandle -eq [IntPtr]::Zero; $i++) { Start-Sleep -Milliseconds 100; $p.Refresh() }
    $hwnd = $p.MainWindowHandle
    $edit = [E2E.Native]::FindWindowEx($hwnd, [IntPtr]::Zero, "Edit", [NullString]::Value)
    # Topmost, top left: other windows cannot cover it, and it does not cover the Scratch Pad.
    [E2E.Native]::SetWindowPos($hwnd, [IntPtr](-1), 100, 100, 1100, 600, 0x40) | Out-Null
    [pscustomobject]@{ Process = $p; Hwnd = $hwnd; Edit = $edit }
}

function Set-Foreground($np) {
    [E2E.Native]::ShowWindow($np.Hwnd, 9) | Out-Null # SW_RESTORE
    if ([E2E.Native]::GetForegroundWindow() -eq $np.Hwnd) { return $true }
    $fg = [E2E.Native]::GetForegroundWindow()
    $pidOut = 0
    $fgThread = [E2E.Native]::GetWindowThreadProcessId($fg, [ref]$pidOut)
    $me = [E2E.Native]::GetCurrentThreadId()
    [E2E.Native]::AttachThreadInput($me, $fgThread, $true) | Out-Null
    [E2E.Native]::BringWindowToTop($np.Hwnd) | Out-Null
    [E2E.Native]::SetForegroundWindow($np.Hwnd) | Out-Null
    [E2E.Native]::AttachThreadInput($me, $fgThread, $false) | Out-Null
    Start-Sleep -Milliseconds 200
    if ([E2E.Native]::GetForegroundWindow() -eq $np.Hwnd) { return $true }
    # The foreground lock refuses SetForegroundWindow from a background process. A real click in
    # the Notepad text area gets the focus; click only if that point shows the Notepad Edit control.
    $r = New-Object E2E.Native+RECT
    [E2E.Native]::GetWindowRect($np.Edit, [ref]$r) | Out-Null
    $x = [int](($r.Left + $r.Right) / 2); $y = [int](($r.Top + $r.Bottom) / 2)
    $pt = New-Object E2E.Native+POINT; $pt.X = $x; $pt.Y = $y
    if ([E2E.Native]::WindowFromPoint($pt) -ne $np.Edit) { return $false }
    $old = New-Object E2E.Native+POINT; [E2E.Native]::GetCursorPos([ref]$old) | Out-Null
    [E2E.Native]::SetCursorPos($x, $y) | Out-Null
    [E2E.Native]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero); [E2E.Native]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 150
    [E2E.Native]::SetCursorPos($old.X, $old.Y) | Out-Null
    Start-Sleep -Milliseconds 150
    [E2E.Native]::GetForegroundWindow() -eq $np.Hwnd
}

function Get-NotepadText($np) {
    $sb = New-Object System.Text.StringBuilder 65536
    [E2E.Native]::SendMessage($np.Edit, 0x0D, [IntPtr]65536, $sb) | Out-Null # WM_GETTEXT
    $sb.ToString()
}

function Clear-NotepadText($np) {
    [E2E.Native]::SendMessage($np.Edit, 0x0C, [IntPtr]::Zero, "") | Out-Null # WM_SETTEXT
}

function Close-TestNotepad($np) { $np.Process | Stop-Process -Force -ErrorAction SilentlyContinue }

function Get-PadState {
    $pad = [E2E.Native]::FindWindow("LiveTyperScratchPad", [NullString]::Value)
    if ($pad -eq [IntPtr]::Zero) { return [pscustomobject]@{ Found = $false } }
    $r = New-Object E2E.Native+RECT
    [E2E.Native]::GetWindowRect($pad, [ref]$r) | Out-Null
    [pscustomobject]@{
        Found = $true; Visible = [E2E.Native]::IsWindowVisible($pad)
        Left = $r.Left; Top = $r.Top; Width = $r.Right - $r.Left; Height = $r.Bottom - $r.Top
    }
}

function Save-Screenshot([string]$path) {
    $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
    $g.Dispose(); $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
    $path
}

<#
  Holds Ctrl+Space for $HoldMs in the test Notepad. $Actions is a hashtable of
  millisecond offset -> scriptblock (for example screenshots or Escape). Returns timings and
  foreground checks.
#>
function Invoke-TalkKey($np, [int]$HoldMs, [hashtable]$Actions = @{}) {
    if (-not (Set-Foreground $np)) { throw "Notepad is not the foreground window: no keys injected." }
    $result = [ordered]@{ ForegroundLost = $false; ForegroundChecks = @(); PadStates = @(); Aborted = $false }
    $sw = [Diagnostics.Stopwatch]::StartNew()
    foreach ($m in $script:Mods) { Key $m $false }; Key $VK_SPACE $false
    $pending = [System.Collections.Generic.SortedDictionary[int, scriptblock]]::new()
    foreach ($k in $Actions.Keys) { $pending[[int]$k] = $Actions[$k] }
    try {
        while ($sw.ElapsedMilliseconds -lt $HoldMs) {
            $fgOk = [E2E.Native]::GetForegroundWindow() -eq $np.Hwnd
            if (-not $fgOk) {
                $result.ForegroundLost = $true; $result.Aborted = $true
                Key $VK_ESCAPE $false; Key $VK_ESCAPE $true
                break
            }
            $due = @($pending.Keys | Where-Object { $_ -le $sw.ElapsedMilliseconds })
            foreach ($t in $due) {
                $result.ForegroundChecks += [pscustomobject]@{ AtMs = $sw.ElapsedMilliseconds; NotepadForeground = $fgOk }
                $result.PadStates += Get-PadState
                & $pending[$t]
                $pending.Remove($t) | Out-Null
            }
            Start-Sleep -Milliseconds 20
        }
    } finally {
        Key $VK_SPACE $true; foreach ($m in $script:Mods) { Key $m $true }
    }
    $result.HeldMs = $sw.ElapsedMilliseconds
    [pscustomobject]$result
}

function Wait-ForPaste($np, [int]$TimeoutMs = 15000) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($sw.ElapsedMilliseconds -lt $TimeoutMs) {
        $t = Get-NotepadText $np
        if ($t.Length -gt 0) { Start-Sleep -Milliseconds 500; return [pscustomobject]@{ Text = (Get-NotepadText $np); AfterReleaseMs = $sw.ElapsedMilliseconds } }
        Start-Sleep -Milliseconds 50
    }
    [pscustomobject]@{ Text = ""; AfterReleaseMs = $null }
}

function Get-Words([string]$s) {
    @(($s.ToLowerInvariant() -replace "[^a-z0-9' ]", " ") -split "\s+" | Where-Object { $_ })
}

function Get-WordErrorRate([string]$expected, [string]$actual) {
    $r = Get-Words $expected; $h = Get-Words $actual
    $d = New-Object 'int[,]' ($r.Count + 1), ($h.Count + 1)
    for ($i = 0; $i -le $r.Count; $i++) { $d[$i, 0] = $i }
    for ($j = 0; $j -le $h.Count; $j++) { $d[0, $j] = $j }
    for ($i = 1; $i -le $r.Count; $i++) {
        for ($j = 1; $j -le $h.Count; $j++) {
            $c = if ($r[$i - 1] -eq $h[$j - 1]) { 0 } else { 1 }
            $d[$i, $j] = [math]::Min([math]::Min($d[($i - 1), $j] + 1, $d[$i, ($j - 1)] + 1), $d[($i - 1), ($j - 1)] + $c)
        }
    }
    [pscustomobject]@{ Errors = $d[$r.Count, $h.Count]; RefWords = $r.Count; WER = [math]::Round($d[$r.Count, $h.Count] / [math]::Max(1, $r.Count), 3) }
}

function Get-WavSeconds([string]$path) { ((Get-Item $path).Length - 44) / 32000.0 }

<# One full Dictation of the whole WAV: sentinel clipboard, hold, paste, checks. #>
function Test-Dictation([string]$wavPath, [string]$label, [int]$shots = 0, [int]$ExtraHoldMs = 1500) {
    $expected = (Get-Content ([IO.Path]::ChangeExtension($wavPath, ".txt")) -Raw).Trim()
    $holdMs = [int]((Get-WavSeconds $wavPath) * 1000) + $ExtraHoldMs
    Set-Clipboard "SENTINEL-123"
    $np = Start-TestNotepad
    $from = Get-LogLineCount
    $actions = @{}
    for ($k = 1; $k -le $shots; $k++) {
        $at = [int]($holdMs * $k / ($shots + 1)); $file = Join-Path $OutDir "$label-shot$k.png"
        $actions[$at] = [scriptblock]::Create("Save-Screenshot '$file' | Out-Null")
    }
    $hold = Invoke-TalkKey $np $holdMs $actions
    $paste = Wait-ForPaste $np
    Start-Sleep -Seconds 2
    $clip = Get-Clipboard -Raw
    $mem = Get-AppMemory
    $wer = Get-WordErrorRate $expected $paste.Text
    Close-TestNotepad $np
    [pscustomobject]@{
        Case = $label; HeldMs = $hold.HeldMs; ForegroundLost = $hold.ForegroundLost
        ForegroundChecks = $hold.ForegroundChecks; PadStates = $hold.PadStates
        Pasted = $paste.Text; PasteSeenAfterReleaseMs = $paste.AfterReleaseMs
        Expected = $expected; WER = $wer; ClipboardAfter = $clip; ClipboardRestored = ($clip -eq "SENTINEL-123")
        Memory = $mem; Log = Get-LogSince $from; PadAfter = Get-PadState
    }
}

function Test-ShortTap {
    Set-Clipboard "SENTINEL-123"
    $np = Start-TestNotepad; $from = Get-LogLineCount
    $hold = Invoke-TalkKey $np 150
    Start-Sleep -Seconds 3
    $r = [pscustomobject]@{ Case = "tap"; HeldMs = $hold.HeldMs; Pasted = (Get-NotepadText $np); Pad = Get-PadState; Clipboard = (Get-Clipboard -Raw); Log = Get-LogSince $from }
    Close-TestNotepad $np; $r
}

function Test-Escape {
    Set-Clipboard "SENTINEL-123"
    $np = Start-TestNotepad; $from = Get-LogLineCount
    # Ctrl+Escape would open the Start menu (Escape is registered without modifiers), so Ctrl
    # goes up first while Space stays down: the Talk Key release check polls Space only.
    $hold = Invoke-TalkKey $np 4000 @{
        2500 = { foreach ($m in $script:Mods) { Key $m $true } }
        3000 = { Key $VK_ESCAPE $false; Start-Sleep -Milliseconds 60; Key $VK_ESCAPE $true }
    }
    Start-Sleep -Seconds 5
    $r = [pscustomobject]@{ Case = "escape"; HeldMs = $hold.HeldMs; Pasted = (Get-NotepadText $np); Pad = Get-PadState; Clipboard = (Get-Clipboard -Raw); Log = Get-LogSince $from }
    Close-TestNotepad $np; $r
}

function Test-ImageClipboard([string]$wavPath) {
    $bmp = New-Object System.Drawing.Bitmap 32, 32
    $g = [System.Drawing.Graphics]::FromImage($bmp); $g.Clear([System.Drawing.Color]::Red); $g.Dispose()
    [System.Windows.Forms.Clipboard]::SetImage($bmp)
    $before = [System.Windows.Forms.Clipboard]::ContainsImage()
    $formatsBefore = [System.Windows.Forms.Clipboard]::GetDataObject().GetFormats()
    $np = Start-TestNotepad; $from = Get-LogLineCount
    $holdMs = [int]((Get-WavSeconds $wavPath) * 1000) + 1500
    $hold = Invoke-TalkKey $np $holdMs
    $paste = Wait-ForPaste $np
    Start-Sleep -Seconds 2
    $after = [System.Windows.Forms.Clipboard]::ContainsImage()
    $img = [System.Windows.Forms.Clipboard]::GetImage()
    $pixel = if ($img) { $img.GetPixel(5, 5).Name } else { $null }
    $r = [pscustomobject]@{
        Case = "image"; HeldMs = $hold.HeldMs; ContainsImageBefore = $before; ContainsImageAfter = $after
        PixelAfter = $pixel; ImageSizeAfter = $(if ($img) { "$($img.Width)x$($img.Height)" })
        FormatsBefore = $formatsBefore -join ", "; FormatsAfter = ([System.Windows.Forms.Clipboard]::GetDataObject().GetFormats() -join ", ")
        TextOnClipboard = [System.Windows.Forms.Clipboard]::ContainsText(); Pasted = $paste.Text; Log = Get-LogSince $from
    }
    Close-TestNotepad $np; $r
}

if ($NoRun) { return }

$results = [ordered]@{}
$from = Get-LogLineCount
Set-TalkKey $TalkKey
Start-App $Wav | Out-Null
if ($Cases -contains "idle") { Start-Sleep -Seconds 3; $results.idle = Get-AppMemory } else { Start-Sleep -Seconds 2 }
if ($Cases -contains "dictation") { $results.dictation = Test-Dictation $Wav "cold" $Screenshots; $results.loadedIdle = Get-AppMemory }
if ($Cases -contains "warm") { $results.warm = Test-Dictation $Wav "warm" 0 }
if ($Cases -contains "tap") { $results.tap = Test-ShortTap }
if ($Cases -contains "escape") { $results.escape = Test-Escape }
if ($Cases -contains "image") { $results.image = Test-ImageClipboard $Wav }
if ($Cases -contains "quit") { $results.quit = Stop-App; Start-Sleep -Milliseconds 500; $results.quitLog = Get-LogSince $from | Select-Object -Last 3 }
if ($Cases -contains "quit") { Restore-Settings }
$json = $results | ConvertTo-Json -Depth 6
$json | Set-Content (Join-Path $OutDir "results.json")
$json
