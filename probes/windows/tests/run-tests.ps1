# Regression tests for the KVMFlow Windows probe scripts.
#
# Runs the REAL scripts (UsbProbe.ps1 unmodified; DisplayProbe.ps1 with only
# the four [KvmProbe.Native]::* call sites swapped for mock functions defined
# in display-mocks.ps1) in fresh pwsh child sessions, then asserts on the
# JSONL session logs they produce.
#
# Covers the paths that were bypassed when only 0-monitor / 0-device machines
# had been used for validation (the real-Windows failures of 2026-09-13):
#   - single-record array unwrapping (count must be 1, not the key count)
#   - $vcp vs $Vcp case-insensitive parameter clobber (2nd monitor iteration)
#   - $pid read-only automatic variable assignment
#   - snapshot and watch modes with 1 and 2 devices, connect/disconnect events
#
# Usage:  pwsh -NoProfile -File .\run-tests.ps1
# Requires PowerShell 7 (pwsh) so the suite runs identically on macOS and
# Windows development machines. The hardware-facing scripts themselves still
# target Windows PowerShell 5.1.

param([switch]$KeepArtifacts)

$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$win = Split-Path -Parent $here
$tmp = Join-Path $here "_artifacts"
Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $tmp | Out-Null

$script:Failures = 0
$script:Passes = 0

function Assert-True {
    param($Condition, [string]$Name)
    if ($Condition) { Write-Host "  PASS  $Name"; $script:Passes++ }
    else { Write-Host "  FAIL  $Name"; $script:Failures++ }
}

function Read-Events {
    param([string]$LogPath)
    $events = @()
    foreach ($line in (Get-Content -LiteralPath $LogPath)) {
        if ($line.Trim() -eq "") { continue }
        $events += ($line | ConvertFrom-Json)
    }
    return ,$events
}

function Find-Event {
    param($Events, [string]$Name)
    return @($Events | Where-Object { $_.event -eq $Name })
}

# null-safe first-match lookup: a broken build may emit no event of a kind,
# and the suite should report FAIL, not crash mid-run
function Get-First {
    param($Events, [string]$Name)
    $m = Find-Event $Events $Name
    if ($m.Count -gt 0) { return $m[0] }
    return $null
}

function Invoke-UsbScenario {
    param([string]$Name, [string[]]$ScriptArgs)
    $log = Join-Path $tmp "$Name.jsonl"
    $inner = ". '$here/usb-mocks.ps1'; & '$win/UsbProbe.ps1' " + ($ScriptArgs -join ' ') + " -LogPath '$log'"
    & pwsh -NoProfile -Command $inner *> (Join-Path $tmp "$Name.stdout")
    return $log
}

function Invoke-DisplayScenario {
    param([string]$Name, [int]$Monitors, [string[]]$ScriptArgs)
    $env:KVM_TEST_MONITORS = "$Monitors"
    $log = Join-Path $tmp "$Name.jsonl"
    $copy = Join-Path $tmp "DisplayProbe.mock.ps1"
    $src = Get-Content -Raw (Join-Path $win "DisplayProbe.ps1")
    # Literal, exact-text swaps of the four native call sites for parenthesized
    # mock command calls (valid in both expression and -not (...) positions).
    $mocked = $src.
        Replace('[KvmProbe.Native]::Collect()', '(Get-TestCollect)').
        Replace('[KvmProbe.Native]::EnumDisplayDevicesA($devName, [uint32]$j, [ref]$dd, 0)', '(Get-TestEnumDisplayDevices $devName $j ([ref]$dd) 0)').
        Replace('[KvmProbe.Native]::GetVcp($Handle, [byte]$Code)', '(Get-TestGetVcp $Handle $Code)').
        Replace('[KvmProbe.Native]::SetVcp($r.handle, [byte]$Vcp, [uint32]$Value)', '(Get-TestSetVcp $r.handle $Vcp $Value)')
    if ($mocked -cmatch '\[KvmProbe\.Native\]::(Collect\(\)|EnumDisplayDevicesA\(|GetVcp\(|SetVcp\()') {
        throw "test harness: native call-site replacement failed - update the .Replace() patterns"
    }
    Set-Content -LiteralPath $copy -Value $mocked
    $inner = ". '$here/display-mocks.ps1'; & '$copy' " + ($ScriptArgs -join ' ') + " -LogPath '$log'"
    & pwsh -NoProfile -Command $inner *> (Join-Path $tmp "$Name.stdout")
    return $log
}

# ---------------------------------------------------------------------------
Write-Host "== DisplayProbe: probe mode, 1 monitor (single-record unwrap regression) =="
$env:KVM_TEST_VCP_CURRENT = "15"
$log = Invoke-DisplayScenario "display-probe-1" 1 @("-Mode", "probe")
$ev = Read-Events $log
Assert-True ((Find-Event $ev 'session.error').Count -eq 0) "no session.error"
Assert-True ((Get-First $ev "display.enumerate").count -eq 1) "enumerate count == 1 (buggy build reported 11 = key count)"
$infos = Find-Event $ev 'display.info'
Assert-True ($infos.Count -eq 1) "exactly one display.info"
Assert-True ($infos[0].edid_id -eq 'SAC-2763-S:0000000000001') "edid_id from WMI = SAC-2763-S:0000000000001 (parity with real mac-side monitor)"
Assert-True ((Get-First $ev "session.start").version -eq "0.1.4") "probe version is 0.1.4"
$reads = Find-Event $ev 'display.ddc.read'
Assert-True ($reads.Count -eq 1 -and $reads[0].result -eq 'ok' -and $reads[0].current -eq 15) "one ddc.read ok current=15"

Write-Host "== DisplayProbe: probe mode, 2 monitors (`$vcp/`$Vcp clobber regression) =="
$log = Invoke-DisplayScenario "display-probe-2" 2 @("-Mode", "probe")
$ev = Read-Events $log
Assert-True ((Find-Event $ev 'session.error').Count -eq 0) "no session.error (buggy build crashed on 2nd monitor)"
Assert-True ((Get-First $ev "display.enumerate").count -eq 2) "enumerate count == 2"
$infos = Find-Event $ev 'display.info'
Assert-True ($infos.Count -eq 2) "two display.info events"
Assert-True (($infos | Where-Object { $_.edid_id -eq 'SAC-2763-S:0000000000001' }).Count -eq 1 -and ($infos | Where-Object { $_.edid_id -eq 'SAC-2466-S:0000000000000' }).Count -eq 1) "both edid_ids distinct and correct (hwid matching)"
$reads = Find-Event $ev 'display.ddc.read'
Assert-True ($reads.Count -eq 2 -and ($reads | Where-Object { $_.result -eq 'ok' }).Count -eq 2) "two ddc.read ok"

Write-Host "== DisplayProbe: inverted WMI EDID auto-correction =="
$env:KVM_TEST_EDID_INVERT = "1"
$log = Invoke-DisplayScenario "display-edid-invert" 1 @("-Mode", "probe")
$ev = Read-Events $log
Assert-True ((Find-Event $ev 'session.error').Count -eq 0) "no session.error"
$infos = Find-Event $ev 'display.info'
Assert-True ($infos.Count -eq 1 -and $infos[0].edid_id -eq 'SAC-2763-S:0000000000001') "bitwise-inverted EDID detected and corrected"
Remove-Item Env:KVM_TEST_EDID_INVERT -ErrorAction SilentlyContinue

Write-Host "== DisplayProbe: probe mode, 2 monitors with FIRST handle value 0 (reads go through, diag isolated) =="
$env:KVM_TEST_ZERO_HANDLE = "first"
$log = Invoke-DisplayScenario "display-mixed-handle" 2 @("-Mode", "probe")
$ev = Read-Events $log
Assert-True ((Find-Event $ev 'session.error').Count -eq 0) "no session.error"
Assert-True ((Get-First $ev "display.enumerate").count -eq 2) "enumerate count == 2"
$reads = Find-Event $ev 'display.ddc.read'
Assert-True (($reads | Where-Object { $_.display_index -eq 0 -and $_.result -eq 'ok' -and $_.handle_note -eq 'handle_value_zero' }).Count -eq 1) "index 0 reads THROUGH handle value 0 with annotation (0.1.3 wrongly refused it)"
Assert-True (($reads | Where-Object { $_.display_index -eq 1 -and $_.result -eq 'ok' -and -not $_.handle_note }).Count -eq 1) "index 1 reads ok without note"
Remove-Item Env:KVM_TEST_ZERO_HANDLE -ErrorAction SilentlyContinue

Write-Host "== DisplayProbe: G73 real signature (16/14) reported as ok with advisory =="
$env:KVM_TEST_VCP_FAKE = "1"
$log = Invoke-DisplayScenario "display-g73-signature" 1 @("-Mode", "probe")
$ev = Read-Events $log
Assert-True ((Find-Event $ev 'session.error').Count -eq 0) "no session.error"
$reads = Find-Event $ev 'display.ddc.read'
Assert-True ($reads.Count -eq 1 -and $reads[0].result -eq 'ok') "result=ok (the 16/14 pair is real G73 data, not a fake signature)"
Assert-True ($reads[0].current -eq 16 -and $reads[0].max -eq 14 -and $reads[0].advisory -eq 'current_gt_max') "values preserved with current_gt_max advisory for review"
$log = Invoke-DisplayScenario "display-g73-getvcp" 1 @("-Mode", "getvcp", "-DisplayIndex", "0")
$ev = Read-Events $log
$reads = Find-Event $ev 'display.ddc.read'
Assert-True ($reads.Count -eq 1 -and $reads[0].result -eq 'ok' -and $reads[0].advisory -eq 'current_gt_max') "getvcp mode reports ok + advisory"
Remove-Item Env:KVM_TEST_VCP_FAKE -ErrorAction SilentlyContinue

Write-Host "== DisplayProbe: all-zero handles still read through =="
$env:KVM_TEST_ZERO_HANDLE = "1"
$log = Invoke-DisplayScenario "display-zero-handle" 1 @("-Mode", "probe")
$ev = Read-Events $log
Assert-True ((Find-Event $ev 'session.error').Count -eq 0) "no session.error"
$reads = Find-Event $ev 'display.ddc.read'
Assert-True ($reads.Count -eq 1 -and $reads[0].result -eq 'ok' -and $reads[0].handle_note -eq 'handle_value_zero') "zero handle reads with visible note"
Assert-True ((Find-Event $ev 'display.info').Count -eq 1) "display.info still emitted for the record"
Remove-Item Env:KVM_TEST_ZERO_HANDLE -ErrorAction SilentlyContinue

Write-Host "== DisplayProbe: current>max becomes advisory, not rejection =="
$env:KVM_TEST_VCP_CURRENT = "30"
$log = Invoke-DisplayScenario "display-current-gt-max" 1 @("-Mode", "probe")
$ev = Read-Events $log
$reads = Find-Event $ev 'display.ddc.read'
Assert-True ($reads.Count -eq 1 -and $reads[0].result -eq 'ok' -and $reads[0].advisory -eq 'current_gt_max') "current(30)>max(27) -> ok + advisory"
Remove-Item Env:KVM_TEST_VCP_CURRENT -ErrorAction SilentlyContinue
$env:KVM_TEST_VCP_CURRENT = "15"

Write-Host "== DisplayProbe: getvcp mode =="
$log = Invoke-DisplayScenario "display-getvcp" 2 @("-Mode", "getvcp", "-DisplayIndex", "1")
$ev = Read-Events $log
Assert-True ((Find-Event $ev 'session.error').Count -eq 0) "no session.error"
$reads = Find-Event $ev 'display.ddc.read'
Assert-True ($reads.Count -eq 1 -and $reads[0].display_index -eq 1 -and $reads[0].current -eq 15) "ddc.read for index 1, current=15"

Write-Host "== DisplayProbe: setvcp mode =="
$log = Invoke-DisplayScenario "display-setvcp" 1 @("-Mode", "setvcp", "-DisplayIndex", "0", "-Value", "16")
$ev = Read-Events $log
Assert-True ((Find-Event $ev 'session.error').Count -eq 0) "no session.error"
$writes = Find-Event $ev 'display.ddc.write'
Assert-True ($writes.Count -eq 1 -and $writes[0].result -eq 'ok') "ddc.write ok"
Assert-True ($writes[0].previous -eq 15 -and $writes[0].read_back -eq 15) "ddc.write previous=15 read_back=15"

# ---------------------------------------------------------------------------
Write-Host "== UsbProbe: snapshot, 1 device (single-device unwrap regression) =="
$env:KVM_TEST_USB_SCENARIO = "one"
$log = Invoke-UsbScenario "usb-snapshot-1" @("-Mode", "snapshot")
$ev = Read-Events $log
Assert-True ((Find-Event $ev 'session.error').Count -eq 0) "no session.error"
Assert-True ((Get-First $ev "usb.snapshot").count -eq 1) "snapshot count == 1 (buggy build reported 12 = key count)"
$devs = Find-Event $ev 'usb.device'
Assert-True ($devs.Count -eq 1 -and $devs[0].vid_pid -eq '046d:c52b') "one usb.device vid_pid=046d:c52b"

Write-Host "== UsbProbe: snapshot, 2 devices (`$pid read-only regression) =="
$env:KVM_TEST_USB_SCENARIO = "two"
$log = Invoke-UsbScenario "usb-snapshot-2" @("-Mode", "snapshot")
$ev = Read-Events $log
Assert-True ((Find-Event $ev 'session.error').Count -eq 0) "no session.error (buggy build threw VariableNotWritable PID)"
Assert-True ((Get-First $ev "usb.snapshot").count -eq 2) "snapshot count == 2"
$devs = Find-Event $ev 'usb.device'
Assert-True ($devs.Count -eq 2) "two usb.device events"
Assert-True (($devs | Where-Object { $_.vid_pid -eq '046d:c52b' }).Count -eq 1 -and ($devs | Where-Object { $_.vid_pid -eq '1a57:0201' }).Count -eq 1) "vid_pids 046d:c52b and 1a57:0201"

Write-Host "== UsbProbe: watch with device churn =="
$env:KVM_TEST_USB_SCENARIO = "churn"
$log = Invoke-UsbScenario "usb-watch-churn" @("-Mode", "watch", "-DurationSec", "3", "-PollMs", "400")
$ev = Read-Events $log
Assert-True ((Find-Event $ev 'session.error').Count -eq 0) "no session.error"
Assert-True ((Get-First $ev "usb.watch.start").known_devices -eq 2) "watch started with 2 known devices"
Assert-True ((Find-Event $ev 'usb.watch.end').Count -eq 1) "watch.end present"
$cons = Find-Event $ev 'usb.connect'
$disc = Find-Event $ev 'usb.disconnect'
Assert-True ($cons.Count -eq 1 -and $cons[0].vid_pid -eq '045e:00db') "one usb.connect vid_pid=045e:00db"
Assert-True ($disc.Count -eq 1 -and $disc[0].vid_pid -eq '045e:00db') "one usb.disconnect vid_pid=045e:00db"

Write-Host "== UsbProbe: diff mode =="
$before = Join-Path $tmp "diff-before.json"
$after = Join-Path $tmp "diff-after.json"
@'
{"count":2,"devices":[{"key":"046d:c52b:ABC123","vid":1133,"pid":50475,"product":"Logitech USB Receiver"},{"key":"045e:00db:DEF456","vid":1118,"pid":219,"product":"Microsoft Keyboard"}]}
'@ | Set-Content -LiteralPath $before
@'
{"count":2,"devices":[{"key":"046d:c52b:ABC123","vid":1133,"pid":50475,"product":"Logitech USB Receiver"},{"key":"1a57:0201:5&2ad2f7e&0&1234","vid":6743,"pid":513,"product":"USB Switch Hub"}]}
'@ | Set-Content -LiteralPath $after
$log = Invoke-UsbScenario "usb-diff" @("-Mode", "diff", "-Before", $before, "-After", $after)
$ev = Read-Events $log
$diff = Find-Event $ev 'usb.diff'
Assert-True ($diff.Count -eq 1 -and $diff[0].added -eq 1 -and $diff[0].removed -eq 1 -and $diff[0].unchanged -eq 1) "diff added=1 removed=1 unchanged=1"

Write-Host "== UsbProbe: diff with non-ASCII device names (PS 5.1 ANSI-read regression) =="
# Snapshots are written UTF-8 without BOM exactly like -Mode snapshot does;
# PowerShell 5.1's Get-Content read them as ANSI and corrupted 中文 product
# names (real-Windows failure at byte 1752). Load-Snapshot now reads UTF-8
# explicitly - this scenario keeps non-ASCII content on the tested path.
$beforeCN = Join-Path $tmp "diff-cn-before.json"
$afterCN = Join-Path $tmp "diff-cn-after.json"
[System.IO.File]::WriteAllText($beforeCN, '{"count":1,"devices":[{"key":"001f:0b26:fixture-composite-1","vid":31,"pid":2854,"product":"USB Composite Device","serial":"fixture-composite-1"}]}')
[System.IO.File]::WriteAllText($afterCN, '{"count":2,"devices":[{"key":"001f:0b26:fixture-composite-1","vid":31,"pid":2854,"product":"USB Composite Device","serial":"fixture-composite-1"},{"key":"067b:2586:fixture-usb-067b-2586-1","vid":1659,"pid":9590,"product":"通用 USB 集线器","serial":"fixture-usb-067b-2586-1"}]}')
$log = Invoke-UsbScenario "usb-diff-cn" @("-Mode", "diff", "-Before", $beforeCN, "-After", $afterCN)
$ev = Read-Events $log
Assert-True ((Find-Event $ev 'session.error').Count -eq 0) "no session.error with non-ASCII device names"
$diff = Find-Event $ev 'usb.diff'
Assert-True ($diff.Count -eq 1 -and $diff[0].added -eq 1 -and $diff[0].removed -eq 0 -and $diff[0].unchanged -eq 1) "diff parses UTF-8 snapshot with 中文 product name (added=1)"

# ---------------------------------------------------------------------------
Remove-Item Env:KVM_TEST_MONITORS -ErrorAction SilentlyContinue
Remove-Item Env:KVM_TEST_VCP_CURRENT -ErrorAction SilentlyContinue
Remove-Item Env:KVM_TEST_USB_SCENARIO -ErrorAction SilentlyContinue

Write-Host ""
Write-Host ("result: {0} passed, {1} failed" -f $script:Passes, $script:Failures)
if ($script:Failures -gt 0 -or $KeepArtifacts) {
    Write-Host "artifacts kept in: $tmp"
} else {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
if ($script:Failures -gt 0) { exit 1 }
exit 0
