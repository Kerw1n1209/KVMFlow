# Regression-test mocks for UsbProbe.ps1.
# Dot-sourced into a fresh pwsh session by run-tests.ps1; mocks Get-CimInstance
# so the REAL UsbProbe.ps1 (unmodified, no textual rewriting needed) runs its
# snapshot/diff/watch paths against fake Win32_PnPEntity records.
#
# Environment knobs:
#   KVM_TEST_USB_SCENARIO  one | two | churn (default two)
#     one   - always a single USB device (single-element unwrap regression)
#     two   - always two devices
#     churn - calls 1-2: two devices; calls 3-4: third device appears;
#             calls 5+: back to two (drives connect+disconnect during watch)

$global:UsbCallCount = 0

function New-TestPnpEntity {
    param([string]$DeviceId, [string]$Name, [string]$PnpClass)
    return [pscustomobject]@{
        DeviceID                 = $DeviceId
        Name                     = $Name
        PNPClass                 = $PnpClass
        Status                   = "OK"
        ConfigManagerErrorCode   = 0
    }
}

function Get-CimInstance {
    param($Namespace, $ClassName, $ErrorAction)
    if ($ClassName -ne "Win32_PnPEntity") { return @() }

    $devs = @()
    $devs += ,(New-TestPnpEntity 'USB\VID_046D&PID_C52B\ABC123' 'Logitech USB Receiver' 'HIDClass')
    $devs += ,(New-TestPnpEntity 'USB\VID_1A57&PID_0201\5&2ad2f7e&0&1234' 'USB Switch Hub' 'USB')

    $scenario = "two"
    if ($env:KVM_TEST_USB_SCENARIO) { $scenario = $env:KVM_TEST_USB_SCENARIO }

    if ($scenario -eq "one") { return ,($devs[0]) }

    if ($scenario -eq "churn") {
        $global:UsbCallCount++
        if ($global:UsbCallCount -le 2) { return ,$devs }
        if ($global:UsbCallCount -le 4) {
            $devs += ,(New-TestPnpEntity 'USB\VID_045E&PID_00DB\DEF456' 'Microsoft Keyboard' 'Keyboard')
            return ,$devs
        }
        return ,$devs
    }

    return ,$devs
}
