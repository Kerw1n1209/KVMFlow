# KVMFlow hardware verification probe - Windows USB side.
# Self-contained PowerShell 5.1 script, no external dependencies.
# Enumeration via Win32_PnPEntity (USB\VID_* device-level entries); watch mode
# is a polling diff (~1s resolution), which is sufficient for USB-switch presses.
#
# Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\UsbProbe.ps1 -Mode snapshot
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\UsbProbe.ps1 -Mode snapshot -OutFile C:\kvm\usb-before.json -LogPath C:\kvm\session.jsonl
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\UsbProbe.ps1 -Mode diff -Before usb-before.json -After usb-after.json
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\UsbProbe.ps1 -Mode watch -DurationSec 120 -LogPath C:\kvm\session.jsonl

param(
    [string]$Mode = "snapshot",    # snapshot | diff | watch
    [string]$OutFile,
    [string]$Before,
    [string]$After,
    [string]$LogPath,
    [int]$DurationSec = 0,         # watch: 0 = run until Ctrl+C
    [int]$PollMs = 700
)

$ErrorActionPreference = "Stop"
$script:Tool = "kvmprobe-win"
$script:Version = "0.1.2"

function Write-Event {
    param([string]$Event, $Fields = @{})
    $o = [ordered]@{}
    $o["ts"] = [DateTime]::UtcNow.ToString("o")
    $o["tool"] = $script:Tool
    $o["version"] = $script:Version
    $o["event"] = $Event
    if ($null -ne $Fields) {
        foreach ($k in $Fields.Keys) { $o[$k] = $Fields[$k] }
    }
    $line = ConvertTo-Json -InputObject $o -Depth 6 -Compress
    Write-Output $line
    if ($LogPath) {
        $logDir = Split-Path -Parent $LogPath
        if ($logDir -and -not (Test-Path -LiteralPath $logDir)) {
            New-Item -ItemType Directory -Path $logDir -Force | Out-Null
        }
        [System.IO.File]::AppendAllText($LogPath, $line + [Environment]::NewLine)
    }
}

# Device-level USB entries only: skip interface children (USB\VID_xxxx&PID_xxxx&MI_nn)
# so the list matches macOS IOUSBDevice-level enumeration.
function Get-UsbDevices {
    $list = @()
    $entities = Get-CimInstance -ClassName Win32_PnPEntity -ErrorAction SilentlyContinue
    foreach ($d in $entities) {
        if ($null -eq $d.DeviceID) { continue }
        if ($d.DeviceID -notlike 'USB\VID_*') { continue }
        if ($d.DeviceID -like '*&MI_*') { continue }

        # $productId, not $pid: $PID is a read-only PowerShell automatic variable
        # (fixed after a real-Windows failure: assignment always throws).
        $vid = -1; $productId = -1; $inst = ""
        if ($d.DeviceID -match '^USB\\VID_([0-9A-Fa-f]{4})&PID_([0-9A-Fa-f]{4})(\\(.+))?$') {
            $vid = [Convert]::ToInt32($Matches[1], 16)
            $productId = [Convert]::ToInt32($Matches[2], 16)
            if ($Matches.Count -ge 5 -and $null -ne $Matches[4]) { $inst = $Matches[4] }
        }
        $list += [ordered]@{
            key      = ('{0:x4}:{1:x4}:{2}' -f $vid, $productId, $inst)
            vid      = $vid
            pid      = $productId
            vid_pid  = ('{0:x4}:{1:x4}' -f $vid, $productId)
            vendor   = ""
            product  = [string]$d.Name
            serial   = $inst
            instance = $inst
            class    = [string]$d.PNPClass
            status   = [string]$d.Status
            cm_error = $d.ConfigManagerErrorCode
            device_id = [string]$d.DeviceID
        }
    }
    # `,$sorted` prevents PowerShell from unwrapping a 1-element array into a
    # single (ordered) hashtable at the pipeline boundary — same defect class
    # that broke DisplayProbe.ps1 on a real Windows machine.
    $sorted = @($list | Sort-Object -Property @{ Expression = { $_.key } })
    return ,$sorted
}

Write-Event "session.start" @{ argv = $MyInvocation.Line; mode = $Mode }

try {
    if ($Mode -eq "snapshot") {
        $devices = Get-UsbDevices
        Write-Event "usb.snapshot" @{ count = $devices.Count }
        foreach ($d in $devices) { Write-Event "usb.device" $d }
        $payload = @{ count = $devices.Count; devices = $devices }
        $json = ConvertTo-Json -InputObject $payload -Depth 6
        if ($OutFile) {
            [System.IO.File]::WriteAllText($OutFile, $json)
            Write-Output ("snapshot written: {0} ({1} devices)" -f $OutFile, $devices.Count)
        }
        Write-Output $json
    }
    elseif ($Mode -eq "diff") {
        if (-not $Before -or -not $After) { throw "diff requires -Before and -After" }
        function Load-Snapshot([string]$Path) {
            $map = @{}
            # ReadAllText with explicit UTF-8: snapshots are written UTF-8 without
            # BOM, but PowerShell 5.1's Get-Content defaults to ANSI and mangles
            # non-ASCII device names (real-Windows failure with 中文 product names).
            $obj = [System.IO.File]::ReadAllText($Path, [System.Text.Encoding]::UTF8) | ConvertFrom-Json
            foreach ($d in $obj.devices) { $map[$d.key] = $d }
            return $map
        }
        $b = Load-Snapshot $Before
        $a = Load-Snapshot $After
        $removed = @(); $added = @(); $unchanged = 0
        foreach ($k in $b.Keys) { if (-not $a.ContainsKey($k)) { $removed += $b[$k] } else { $unchanged++ } }
        foreach ($k in $a.Keys) { if (-not $b.ContainsKey($k)) { $added += $a[$k] } }
        Write-Event "usb.diff" @{ before = $Before; after = $After; added = $added.Count; removed = $removed.Count; unchanged = $unchanged }
        ConvertTo-Json -InputObject @{
            before = $Before; after = $After
            added_count = $added.Count; removed_count = $removed.Count; unchanged_count = $unchanged
            added = $added; removed = $removed
        } -Depth 6
    }
    elseif ($Mode -eq "watch") {
        $script:Prev = @{}
        foreach ($d in (Get-UsbDevices)) { $script:Prev[$d["key"]] = $d }
        Write-Event "usb.watch.start" @{ known_devices = $script:Prev.Count; duration_sec = $DurationSec; poll_ms = $PollMs }

        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        $lastTick = $sw.Elapsed
        while ($true) {
            Start-Sleep -Milliseconds $PollMs
            if ($DurationSec -gt 0 -and $sw.Elapsed.TotalSeconds -ge $DurationSec) { break }

            $cur = @{}
            foreach ($d in (Get-UsbDevices)) { $cur[$d["key"]] = $d }
            foreach ($k in @($cur.Keys)) {
                if (-not $script:Prev.ContainsKey($k)) { Write-Event "usb.connect" $cur[$k] }
            }
            foreach ($k in @($script:Prev.Keys)) {
                if (-not $cur.ContainsKey($k)) { Write-Event "usb.disconnect" $script:Prev[$k] }
            }
            $script:Prev = $cur

            if (($sw.Elapsed - $lastTick).TotalSeconds -ge 10) {
                Write-Event "usb.watch.tick" @{}
                $lastTick = $sw.Elapsed
            }
        }
        Write-Event "usb.watch.end" @{}
    }
    else {
        throw "unknown mode: $Mode (snapshot | diff | watch)"
    }
}
catch {
    Write-Event "session.error" @{ error = $_.Exception.Message }
    throw
}
finally {
    Write-Event "session.end" @{}
}
