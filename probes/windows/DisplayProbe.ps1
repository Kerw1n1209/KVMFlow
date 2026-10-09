# KVMFlow hardware verification probe - Windows display/DDC side.
# Self-contained PowerShell 5.1 script, no external dependencies.
# DDC goes through dxva2.dll (GetVCPFeatureAndVCPFeatureReply / SetVCPFeature).
#
# Usage (run in an elevated-or-normal PowerShell; no admin required for DDC/WMI):
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\DisplayProbe.ps1 -Mode probe
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\DisplayProbe.ps1 -Mode probe -LogPath C:\kvm\session.jsonl
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\DisplayProbe.ps1 -Mode getvcp -DisplayIndex 0
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\DisplayProbe.ps1 -Mode setvcp -DisplayIndex 0 -Value 15
#
# IMPORTANT: setvcp changes the physical monitor input. Only use during the
# human hardware session with the operator ready to confirm/restore the picture.

param(
    [string]$Mode = "probe",      # probe | getvcp | setvcp
    [int]$DisplayIndex = -1,      # 0-based index as printed by -Mode probe
    [int]$Vcp = 0x60,
    [int]$Value = -1,
    [string]$LogPath
)

$ErrorActionPreference = "Stop"
$script:Tool = "kvmprobe-win"
$script:Version = "0.1.4"

function Write-Event {
    param([string]$Event, [hashtable]$Fields = @{})
    $o = [ordered]@{}
    $o["ts"] = [DateTime]::UtcNow.ToString("o")
    $o["tool"] = $script:Tool
    $o["version"] = $script:Version
    $o["event"] = $Event
    foreach ($k in $Fields.Keys) { $o[$k] = $Fields[$k] }
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

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Collections.Generic;

namespace KvmProbe {
    public class Native {
        [StructLayout(LayoutKind.Sequential)]
        public struct RECT { public int Left, Top, Right, Bottom; }

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Ansi)]
        public struct MONITORINFOEX {
            public int cbSize;
            public RECT rcMonitor;
            public RECT rcWork;
            public uint dwFlags;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)]
            public string szDevice;
        }

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Ansi)]
        public struct DISPLAY_DEVICE {
            public int cb;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 32)] public string DeviceName;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string DeviceString;
            public uint StateFlags;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string DeviceID;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string DeviceKey;
        }

        // szPhysicalMonitorDescription is WCHAR[128] in the native API —
        // marshaling as Ansi truncated it to one char and left handle=0 on a
        // real Windows 11 box (heap overflow from the 136 vs 264 byte layout).
        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        public struct PHYSICAL_MONITOR {
            public IntPtr hPhysicalMonitor;
            [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)]
            public string szPhysicalMonitorDescription;
        }

        public delegate bool MonitorEnumProc(IntPtr hMonitor, IntPtr hdcMonitor, ref RECT lprcMonitor, IntPtr dwData);

        [DllImport("user32.dll", SetLastError = true)]
        public static extern bool EnumDisplayMonitors(IntPtr hdc, IntPtr clip, MonitorEnumProc proc, IntPtr dwData);

        [DllImport("user32.dll", CharSet = CharSet.Ansi, SetLastError = true)]
        public static extern bool GetMonitorInfoA(IntPtr hMonitor, ref MONITORINFOEX info);

        [DllImport("user32.dll", CharSet = CharSet.Ansi, SetLastError = true)]
        public static extern bool EnumDisplayDevicesA(string lpDevice, uint iDevNum, ref DISPLAY_DEVICE lpDisplayDevice, uint dwFlags);

        [DllImport("dxva2.dll", SetLastError = true)]
        public static extern bool GetNumberOfPhysicalMonitorsFromHMONITOR(IntPtr hMonitor, ref uint pdwNumberOfPhysicalMonitors);

        [DllImport("dxva2.dll", SetLastError = true)]
        public static extern bool GetPhysicalMonitorsFromHMONITOR(IntPtr hMonitor, uint dwPhysicalMonitorArraySize, [Out] PHYSICAL_MONITOR[] lpPhysicalMonitorArray);

        [DllImport("dxva2.dll")]
        public static extern bool DestroyPhysicalMonitor(IntPtr hMonitor);

        [DllImport("dxva2.dll", SetLastError = true)]
        public static extern bool GetVCPFeatureAndVCPFeatureReply(IntPtr hMonitor, byte bCode, ref uint pvct, ref uint pdwCurrentValue, ref uint pdwMaximumValue);

        [DllImport("dxva2.dll", SetLastError = true)]
        public static extern bool SetVCPFeature(IntPtr hMonitor, byte bCode, uint dwNewValue);

        // Returns one entry per physical monitor: [deviceName, hPhysicalMonitor,
        // description, perRecordDiag]. LastCollectDiag carries failures that
        // produced no record at all (read it after the call).
        public static string LastCollectDiag = "";
        public static List<object[]> Collect() {
            var list = new List<object[]>();
            LastCollectDiag = "";
            MonitorEnumProc cb = delegate(IntPtr hMon, IntPtr hdc, ref RECT r, IntPtr data) {
                var info = new MONITORINFOEX();
                info.cbSize = Marshal.SizeOf(typeof(MONITORINFOEX));
                if (!GetMonitorInfoA(hMon, ref info)) {
                    LastCollectDiag += "GetMonitorInfoA rc=" + Marshal.GetLastWin32Error() + "; ";
                    return true;
                }
                uint n = 0;
                if (!GetNumberOfPhysicalMonitorsFromHMONITOR(hMon, ref n)) {
                    list.Add(new object[] { info.szDevice, IntPtr.Zero, "",
                        "GetNumberOfPhysicalMonitors rc=" + Marshal.GetLastWin32Error() });
                    return true;
                }
                if (n == 0) {
                    list.Add(new object[] { info.szDevice, IntPtr.Zero, "",
                        "GetNumberOfPhysicalMonitors count=0" });
                    return true;
                }
                var arr = new PHYSICAL_MONITOR[n];
                if (!GetPhysicalMonitorsFromHMONITOR(hMon, n, arr)) {
                    list.Add(new object[] { info.szDevice, IntPtr.Zero, "",
                        "GetPhysicalMonitorsFromHMONITOR rc=" + Marshal.GetLastWin32Error() });
                    return true;
                }
                for (uint i = 0; i < n; i++) {
                    string diag = "";
                    if (arr[i].hPhysicalMonitor == IntPtr.Zero) {
                        // observed on real Windows: the API returns TRUE and the
                        // handle VALUE is 0 - cross-reads proved value 0 can be a
                        // valid handle, so this is a visibility note, not a failure
                        diag = "handle_value_zero";
                    }
                    list.Add(new object[] { info.szDevice, arr[i].hPhysicalMonitor,
                        arr[i].szPhysicalMonitorDescription, diag });
                }
                return true;
            };
            EnumDisplayMonitors(IntPtr.Zero, IntPtr.Zero, cb, IntPtr.Zero);
            return list;
        }

        // [ok, current, max, vcpType, lastWin32Error]
        public static object[] GetVcp(IntPtr h, byte code) {
            uint t = 0, cur = 0, max = 0;
            bool ok = GetVCPFeatureAndVCPFeatureReply(h, code, ref t, ref cur, ref max);
            return new object[] { ok, (int)cur, (int)max, (int)t, Marshal.GetLastWin32Error() };
        }

        // [ok, lastWin32Error]
        public static object[] SetVcp(IntPtr h, byte code, uint value) {
            bool ok = SetVCPFeature(h, code, value);
            return new object[] { ok, Marshal.GetLastWin32Error() };
        }
    }
}
"@

function ConvertFrom-U16Array {
    param($Arr)
    if ($null -eq $Arr) { return "" }
    $s = ""
    foreach ($c in $Arr) {
        if ($c -eq 0) { break }
        $s += [char][int]$c
    }
    return $s
}

# VCP read with advisory annotations instead of hard downgrades.
# Real-Windows cross-read (2026-09-13) proved that dxva2 can hand out handle
# VALUE 0 as a VALID handle (distinct per-monitor brightness values were read
# through handles 0 and 1), so the 0.1.2/0.1.3 "null-handle refusal" and
# "16/14 fake signature" rules rested on a wrong premise and rejected real
# data. Values are now reported as measured, with visible advisory notes;
# only an API failure is an error. Returns
# [ok, current, max, vcpType, rc, advisory] where advisory is "" or
# "max_le_zero" | "current_gt_max" (VCP 60 max is not a cap on many monitors,
# so current_gt_max flags the reading for review, it does not reject it).
function Get-AnnotatedVcp {
    param($Handle, [int]$Code)
    $v = [KvmProbe.Native]::GetVcp($Handle, [byte]$Code)
    if (-not [bool]$v[0]) { return @($v[0], $v[1], $v[2], $v[3], $v[4], "") }
    $advisory = ""
    if ($v[2] -le 0) { $advisory = "max_le_zero" }
    elseif ($v[1] -gt $v[2]) { $advisory = "current_gt_max" }
    return @($v[0], $v[1], $v[2], $v[3], $v[4], $advisory)
}

# Minimal EDID parser: mirrors the macOS probe so both platforms produce the
# same edid_id format for the same physical monitor.
function Get-EdidInfo {
    param([byte[]]$E)
    if ($null -eq $E -or $E.Count -lt 128) { return $null }
    if ($E[0] -ne 0x00 -or $E[1] -ne 0xFF -or $E[7] -ne 0x00) { return $null }
    $sum = 0
    for ($i = 0; $i -lt 128; $i++) { $sum += [int]$E[$i] }
    $checksumOk = (($sum -band 0xFF) -eq 0)

    $b0 = [int]$E[8]; $b1 = [int]$E[9]
    $letters = (64 + (($b0 -shr 2) -band 0x1F)), (64 + ((($b0 -band 3) -shl 3) -bor (($b1 -shr 5) -band 7))), (64 + ($b1 -band 0x1F))
    $mfr = ""
    foreach ($c in $letters) { if ($c -gt 64 -and $c -le 90) { $mfr += [char][int]$c } }
    $prod = [int]$E[10] -bor ([int]$E[11] -shl 8)
    $serial = [uint32]([int]$E[12] -bor ([int]$E[13] -shl 8) -bor ([int]$E[14] -shl 16) -bor ([int]$E[15] -shl 24))

    $modelName = $null
    $serialString = $null
    foreach ($base in @(54, 72, 90, 108)) {
        if ($E[$base] -ne 0 -or $E[$base + 1] -ne 0) { continue }
        $flag = $E[$base + 3]
        $text = ""
        for ($i = $base + 5; $i -lt $base + 18; $i++) {
            if ($E[$i] -eq 0x0A -or $E[$i] -eq 0x00) { break }
            $text += [char][int]$E[$i]
        }
        $text = $text.Trim()
        if ($flag -eq 0xFC -and $null -eq $modelName -and $text -ne "") { $modelName = $text }
        if ($flag -eq 0xFF -and $null -eq $serialString -and $text -ne "") { $serialString = $text }
    }

    if ($serial -ne 0) {
        $edidId = ("{0}-{1:X4}-{2:X8}" -f $mfr, $prod, $serial)
    } elseif ($serialString) {
        $edidId = ("{0}-{1:X4}-S:{2}" -f $mfr, $prod, $serialString)
    } else {
        $edidId = ("{0}-{1:X4}-NOSERIAL" -f $mfr, $prod)
    }

    $hex = ($E[0..127] | ForEach-Object { $_.ToString("x2") }) -join ""
    return @{
        manufacturer   = $mfr
        product_code   = ("{0:X4}" -f $prod)
        serial_number  = [int64]$serial
        model_name     = $modelName
        serial_string  = $serialString
        checksum_ok    = $checksumOk
        edid_id        = $edidId
        edid_hex       = $hex
    }
}

Write-Event "session.start" @{ argv = $MyInvocation.Line; mode = $Mode }

try {
    $wmiIds = Get-CimInstance -Namespace "root/wmi" -ClassName WmiMonitorID -ErrorAction SilentlyContinue
    # WmiGetMonitorRawEEdid gives the real EDID bytes; registry is only a fallback
    $wmiEdid = Get-CimInstance -Namespace "root/wmi" -ClassName WmiMonitorDescriptorMethods -ErrorAction SilentlyContinue
    $script:UsedInstanceNames = @()

    function Get-MonitorRecords {
        $records = @()
        $i = 0
        foreach ($m in [KvmProbe.Native]::Collect()) {
            $devName = [string]$m[0]
            $handle = [IntPtr]$m[1]
            $desc = [string]$m[2]
            $collectDiag = [string]$m[3]

            $dd = New-Object KvmProbe.Native+DISPLAY_DEVICE
            $dd.cb = [Runtime.InteropServices.Marshal]::SizeOf($dd)
            $deviceId = ""
            $deviceString = ""
            for ($j = 0; $j -lt 4; $j++) {
                if (-not [KvmProbe.Native]::EnumDisplayDevicesA($devName, [uint32]$j, [ref]$dd, 0)) { break }
                if ($dd.DeviceID -like 'MONITOR\*') {
                    $deviceId = $dd.DeviceID
                    $deviceString = $dd.DeviceString
                    break
                }
            }

            # WmiMonitorID InstanceName uses the DISPLAY\ prefix; EnumDisplayDevices
            # gives MONITOR\<hwid>\... — match on the shared hardware id segment.
            $hwid = ""
            if ($deviceId -like 'MONITOR\*') { $hwid = $deviceId.Split('\')[1] }
            $wmiMatch = $null
            if ($hwid -ne "") {
                foreach ($w in $wmiIds) {
                    if ($w.InstanceName -like "DISPLAY\$hwid\*" -and $script:UsedInstanceNames -notcontains $w.InstanceName) {
                        $wmiMatch = $w
                        break
                    }
                }
                if ($wmiMatch) { $script:UsedInstanceNames += $wmiMatch.InstanceName }
            }

            $edidSource = "none"
            $edidBytes = $null
            if ($wmiMatch -and $wmiEdid) {
                $descMethod = @($wmiEdid | Where-Object { $_.InstanceName -eq $wmiMatch.InstanceName })[0]
                if ($descMethod) {
                    try {
                        $r = Invoke-CimMethod -InputObject $descMethod -MethodName WmiGetMonitorRawEEdid -ErrorAction Stop
                        if ($r.BlockContent -and $r.BlockContent.Length -ge 128) {
                            $slice = [byte[]]$r.BlockContent[0..127]
                            # WMI sometimes hands back bitwise-inverted EDID; the header tells us
                            if ($slice[0] -eq 0xFF -and $slice[1] -eq 0x00) {
                                $slice = [byte[]]($slice | ForEach-Object { $_ -bxor 0xFF })
                            }
                            if ($slice[0] -eq 0x00 -and $slice[1] -eq 0xFF) {
                                $edidBytes = $slice
                                $edidSource = "wmi"
                            }
                        }
                    } catch { }
                }
            }
            if (-not $edidBytes -and $hwid -ne "") {
                try {
                    $enumBase = "HKLM:\SYSTEM\CurrentControlSet\Enum\DISPLAY\$hwid"
                    foreach ($inst in (Get-ChildItem -Path $enumBase -ErrorAction Stop)) {
                        $props = Get-ItemProperty -Path (Join-Path $inst.PSPath "Device Parameters") -ErrorAction SilentlyContinue
                        if ($props -and $props.PSObject.Properties["EDID"] -and $props.EDID.Length -ge 128) {
                            $edidHex = ($props.EDID | Select-Object -First 128 | ForEach-Object { $_.ToString("x2") }) -join ""
                            $edidBytes = New-Object byte[] 128
                            for ($k = 0; $k -lt 128; $k++) { $edidBytes[$k] = [Convert]::ToByte($edidHex.Substring($k * 2, 2), 16) }
                            $edidSource = "registry"
                            break
                        }
                    }
                } catch { }
            }

            $edidInfo = $null
            if ($edidBytes) { $edidInfo = Get-EdidInfo $edidBytes }

            $records += @{
                index        = $i
                device       = $devName
                description  = $desc
                device_string = $deviceString
                device_id    = $deviceId
                edid_source  = $edidSource
                collect_diag = $collectDiag
                wmi_manufacturer = ConvertFrom-U16Array $(if ($wmiMatch) { $wmiMatch.ManufacturerName } else { $null })
                wmi_product  = ConvertFrom-U16Array $(if ($wmiMatch) { $wmiMatch.ProductCodeID } else { $null })
                wmi_serial   = ConvertFrom-U16Array $(if ($wmiMatch) { $wmiMatch.SerialNumberID } else { $null })
                wmi_name     = ConvertFrom-U16Array $(if ($wmiMatch) { $wmiMatch.UserFriendlyName } else { $null })
                edid         = $edidInfo
                handle       = $handle
            }
            $i++
        }
        # `,$records` prevents PowerShell from unwrapping a 1-element array into
        # a single hashtable at the pipeline boundary (fixed after a real-Windows
        # failure: count reported 11 = key count of one record).
        return ,$records
    }

    if ($Mode -eq "probe") {
        $records = Get-MonitorRecords
        Write-Event "display.enumerate" @{ count = $records.Count; collect_diag = [KvmProbe.Native]::LastCollectDiag }
        $summary = @()
        foreach ($r in $records) {
            $entry = [ordered]@{
                display_index  = $r.index
                device         = $r.device
                physical_monitor_description = $r.description
                device_string  = $r.device_string
                device_id      = $r.device_id
                wmi_manufacturer = $r.wmi_manufacturer
                wmi_product    = $r.wmi_product
                wmi_serial     = $r.wmi_serial
                wmi_user_friendly_name = $r.wmi_name
            }
            if ($r.edid) {
                $entry["edid_id"] = $r.edid.edid_id
                $entry["manufacturer"] = $r.edid.manufacturer
                $entry["product_code"] = $r.edid.product_code
                $entry["serial_number"] = $r.edid.serial_number
                $entry["model_name"] = $r.edid.model_name
                $entry["serial_string"] = $r.edid.serial_string
                $entry["edid_checksum_ok"] = $r.edid.checksum_ok
                $entry["edid_hex"] = $r.edid.edid_hex
            } else {
                $entry["edid_id"] = ""
            }
            $entry["edid_source"] = $r.edid_source

            # Reads go through ANY handle value: 0 proved to be a valid handle
            # on real hardware. The zero value is annotated, not refused.
            $handleNote = ""
            if ($r.handle -eq [IntPtr]::Zero) { $handleNote = "handle_value_zero" }

            # $vcpResult, not $vcp: PowerShell variables are case-insensitive, so a
            # local named $vcp would clobber the $Vcp parameter and break the next
            # [byte]$Vcp cast (fixed after a real-Windows failure on the 2nd monitor).
            $vcpResult = Get-AnnotatedVcp $r.handle $Vcp
            if (-not [bool]$vcpResult[0]) {
                $entry["vcp60_read"] = @{ supported = $false; win32_error = $vcpResult[4] }
                Write-Event "display.ddc.read" @{
                    display_index = $r.index; edid_id = $entry["edid_id"]; vcp = $Vcp; result = "error"
                    rc = $vcpResult[4]; error = "GetVCPFeatureAndVCPFeatureReply_failed"; handle_note = $handleNote
                }
            }
            else {
                $read = @{ supported = $true; current = $vcpResult[1]; max = $vcpResult[2] }
                if ($handleNote) { $read["handle_note"] = $handleNote }
                if ($vcpResult[5]) { $read["advisory"] = $vcpResult[5] }
                $entry["vcp60_read"] = $read
                Write-Event "display.ddc.read" @{
                    display_index = $r.index; edid_id = $entry["edid_id"]; vcp = $Vcp; result = "ok"
                    current = $vcpResult[1]; max = $vcpResult[2]; rc = 0
                    handle_note = $handleNote; advisory = $vcpResult[5]
                }
            }
            Write-Event "display.info" @{
                display_index = $r.index; edid_id = $entry["edid_id"]
                model_name = $entry["model_name"]; device_string = $r.device_string
            }
            $summary += $entry
        }
        ConvertTo-Json -InputObject @{ displays = $summary } -Depth 6
    }
    elseif ($Mode -eq "getvcp") {
        if ($DisplayIndex -lt 0) { throw "getvcp requires -DisplayIndex" }
        $records = Get-MonitorRecords
        $r = $records | Where-Object { $_.index -eq $DisplayIndex }
        if (-not $r) { throw "DisplayIndex $DisplayIndex not found" }
        $handleNote = ""
        if ($r.handle -eq [IntPtr]::Zero) { $handleNote = "handle_value_zero" }
        $vcpResult = Get-AnnotatedVcp $r.handle $Vcp
        $out = [ordered]@{
            display_index = $r.index
            vcp           = $Vcp
            result        = $(if ([bool]$vcpResult[0]) { "ok" } else { "error" })
            current       = $vcpResult[1]; max = $vcpResult[2]; rc = $vcpResult[4]
            handle_note   = $handleNote; advisory = $vcpResult[5]
        }
        Write-Event "display.ddc.read" @{
            display_index = $r.index; vcp = $Vcp
            result = $out["result"]; current = $vcpResult[1]; max = $vcpResult[2]; rc = $vcpResult[4]
            handle_note = $handleNote; advisory = $vcpResult[5]
        }
        ConvertTo-Json -InputObject $out -Depth 4
    }
    elseif ($Mode -eq "setvcp") {
        if ($DisplayIndex -lt 0 -or $Value -lt 0) { throw "setvcp requires -DisplayIndex and -Value" }
        $records = Get-MonitorRecords
        $r = $records | Where-Object { $_.index -eq $DisplayIndex }
        if (-not $r) { throw "DisplayIndex $DisplayIndex not found" }

        $handleNote = ""
        if ($r.handle -eq [IntPtr]::Zero) { $handleNote = "handle_value_zero" }
        $before = Get-AnnotatedVcp $r.handle $Vcp
        $set = [KvmProbe.Native]::SetVcp($r.handle, [byte]$Vcp, [uint32]$Value)
        Start-Sleep -Milliseconds 300
        $after = Get-AnnotatedVcp $r.handle $Vcp
        $out = [ordered]@{
            display_index = $r.index
            vcp           = $Vcp
            value         = $Value
            result        = $(if ([bool]$set[0]) { "ok" } else { "error" })
            rc            = $set[1]
            previous      = $before[1]
            previous_advisory = $before[5]
            read_back     = $after[1]
            read_back_advisory = $after[5]
            handle_note   = $handleNote
            note          = "ddc_ok_is_machine_observed_only_picture_must_be_human_confirmed"
        }
        Write-Event "display.ddc.write" @{
            display_index = $r.index; vcp = $Vcp; value = $Value
            result = $out["result"]; rc = $set[1]
            previous = $before[1]; previous_advisory = $before[5]
            read_back = $after[1]; read_back_advisory = $after[5]
            handle_note = $handleNote
        }
        ConvertTo-Json -InputObject $out -Depth 4
    }
    else {
        throw "unknown mode: $Mode (probe | getvcp | setvcp)"
    }
}
catch {
    Write-Event "session.error" @{ error = $_.Exception.Message }
    throw
}
finally {
    Write-Event "session.end" @{}
}
