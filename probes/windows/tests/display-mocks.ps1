# Regression-test mocks for DisplayProbe.ps1 native call sites.
# Dot-sourced into a fresh pwsh session by run-tests.ps1. The orchestrator
# rewrites the four "[KvmProbe.Native]::*" call sites in a temporary copy of
# the REAL script to plain function names defined here, so everything
# downstream of the native layer (record building, array handling, mode
# dispatch, EDID sourcing, JSONL logging) executes as the real code under test.
#
# Environment knobs:
#   KVM_TEST_MONITORS     number of monitors Collect() reports (default 1)
#   KVM_TEST_VCP_CURRENT  current value returned by GetVcp (default 15)
#   KVM_TEST_VCP_FAKE     1 -> GetVcp returns the real-Windows null-handle fake signature (16/14)
#   KVM_TEST_ZERO_HANDLE  1 -> all records have IntPtr.Zero handles; "first" -> only the first
#   KVM_TEST_EDID_INVERT  1 -> WMI EDID bytes come back bitwise-inverted

$global:Sac2763EdidHex = "00ffffffffffff004c236327000000000522010400000000000000000000000000000000000000000000000000000000000000000000000000fc0054455354204737330000000000000000ff00303030303030303030303030310000000000000000000000000000000000000000000000000000000000000000000000000064"
$global:Sac2466EdidHex = "00ffffffffffff004c236624000000000522010400000000000000000000000000000000000000000000000000000000000000000000000000fc005445535420473532706c757300000000ff003030303030303030303030303000000000000000000000000000000000000000000000000000000000000000000000000000a4"

function Get-TestEdidBytes {
    param([string]$HwId)
    $hex = if ($HwId -eq "SAC2763") { $global:Sac2763EdidHex } else { $global:Sac2466EdidHex }
    $bytes = New-Object byte[] ($hex.Length / 2)
    for ($k = 0; $k -lt $bytes.Length; $k++) { $bytes[$k] = [Convert]::ToByte($hex.Substring($k * 2, 2), 16) }
    if ($env:KVM_TEST_EDID_INVERT -eq "1") {
        for ($k = 0; $k -lt $bytes.Length; $k++) { $bytes[$k] = $bytes[$k] -bxor 0xFF }
    }
    return ,$bytes
}

function Get-TestCollect {
    $n = 1
    if ($env:KVM_TEST_MONITORS) { $n = [int]$env:KVM_TEST_MONITORS }
    if ($n -lt 1) { $n = 1 }
    $zeroMode = ""
    if ($env:KVM_TEST_ZERO_HANDLE) { $zeroMode = $env:KVM_TEST_ZERO_HANDLE }
    $recs = @()
    for ($i = 0; $i -lt $n; $i++) {
        # same shape as the real Collect(): object[] { deviceName, handle, description, diag }
        $handle = [IntPtr]::new(0x10000 + $i)
        $diag = ""
        $isZero = ($zeroMode -eq "1" -or $zeroMode -eq "all") -or ($zeroMode -eq "first" -and $i -eq 0)
        if ($isZero) { $handle = [IntPtr]::Zero; $diag = "physical_monitor_handle_null" }
        $recs += ,@([object[]]@("\\.\DISPLAY$($i + 1)", $handle, "TEST MONITOR $i", $diag))
    }
    return ,$recs
}

function Get-TestEnumDisplayDevices {
    param($devName, $j, $ddRef, $flags)
    if ($j -eq 0) {
        $hwid = "SAC2763"
        if ($devName -like "*DISPLAY2") { $hwid = "SAC2466" }
        $ddRef.Value.DeviceID = "MONITOR\$hwid\{4d36e96e-e325-11ce-bfc1-08002be10318}\000$($j + 6)"
        $ddRef.Value.DeviceString = "TEST Monitor Device ($hwid)"
        return $true
    }
    return $false
}

function Get-TestGetVcp {
    param($handle, $code)
    if ($env:KVM_TEST_VCP_FAKE -eq "1") {
        # the exact garbage a null/invalid handle produced on real Windows
        return @($true, 16, 14, 1, 0)
    }
    $cur = 15
    if ($env:KVM_TEST_VCP_CURRENT) { $cur = [int]$env:KVM_TEST_VCP_CURRENT }
    # same shape as the real GetVcp(): ok, current, max, vcpType, lastWin32Error
    return @($true, $cur, 27, 2, 0)
}

function Get-TestSetVcp {
    param($handle, $code, $value)
    # same shape as the real SetVcp(): ok, lastWin32Error
    return @($true, 0)
}

function Get-CimInstance {
    param($Namespace, $ClassName, $ErrorAction)
    if ($ClassName -eq "WmiMonitorID") {
        $ids = @()
        $ids += ,(New-TestWmiMonitorId "DISPLAY\SAC2763\5&test&0&UID13579" "SAC" "2763" "0000000000001" "TEST G73")
        $ids += ,(New-TestWmiMonitorId "DISPLAY\SAC2466\5&test&0&UID24680" "SAC" "2466" "0000000000000" "TEST G52plus")
        return ,$ids
    }
    if ($ClassName -eq "WmiMonitorDescriptorMethods") {
        $ms = @()
        $ms += ,([pscustomobject]@{ InstanceName = "DISPLAY\SAC2763\5&test&0&UID13579" })
        $ms += ,([pscustomobject]@{ InstanceName = "DISPLAY\SAC2466\5&test&0&UID24680" })
        return ,$ms
    }
    return @()
}

function New-TestWmiMonitorId {
    param([string]$InstanceName, [string]$Mfr, [string]$Product, [string]$Serial, [string]$Name)
    return [pscustomobject]@{
        InstanceName     = $InstanceName
        ManufacturerName = [int[]][char[]]$Mfr
        ProductCodeID    = [int[]][char[]]$Product
        SerialNumberID   = [int[]][char[]]$Serial
        UserFriendlyName = [int[]][char[]]$Name
    }
}

function Invoke-CimMethod {
    param($InputObject, $MethodName, $ErrorAction)
    if ($MethodName -eq "WmiGetMonitorRawEEdid") {
        $hwid = "SAC2466"
        if ($InputObject.InstanceName -like "*SAC2763*") { $hwid = "SAC2763" }
        $bytes = Get-TestEdidBytes $hwid
        return [pscustomobject]@{ BlockContent = $bytes }
    }
    return $null
}
