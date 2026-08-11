$ErrorActionPreference = 'Stop'

$sdkRoot = if ($env:ANDROID_SDK_ROOT) {
    $env:ANDROID_SDK_ROOT
} elseif ($env:ANDROID_HOME) {
    $env:ANDROID_HOME
} else {
    Join-Path $env:LOCALAPPDATA 'Android\Sdk'
}
$script:adb = Join-Path $sdkRoot 'platform-tools\adb.exe'
if (-not (Test-Path -LiteralPath $script:adb -PathType Leaf)) {
    $adbCommand = Get-Command adb -CommandType Application -ErrorAction SilentlyContinue
    if (-not $adbCommand) {
        throw 'Android platform-tools adb.exe is unavailable'
    }
    $script:adb = $adbCommand.Source
}

function Invoke-AdbText {
    param([Parameter(Mandatory = $true)][string[]]$Arguments)

    $previousErrorActionPreference = $ErrorActionPreference
    try {
        # Windows PowerShell promotes native stderr to an ErrorRecord.  ADB
        # legitimately writes its first daemon-start notice there, so capture
        # it without turning a successful cold start into a terminating error.
        $ErrorActionPreference = 'Continue'
        $output = & $script:adb @Arguments 2>&1
        $exitCode = $LASTEXITCODE
    }
    finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }
    if ($exitCode -ne 0) {
        throw "adb command failed"
    }
    return ($output -join "`n").Trim()
}

$adbVersion = Invoke-AdbText -Arguments @('version')
$devices = Invoke-AdbText -Arguments @('devices')
$online = @($devices -split "`n" | Where-Object { $_ -match "\sdevice$" })
if ($online.Count -ne 1) {
    throw "T-110 requires exactly one online physical Android device"
}

$serial = ($online[0] -split "\s+")[0]
$qemu = Invoke-AdbText -Arguments @('-s', $serial, 'shell', 'getprop', 'ro.kernel.qemu')
if ($qemu -eq '1' -or $serial.StartsWith('emulator-')) {
    throw "emulators cannot satisfy the T-110 physical gate"
}

$abis = Invoke-AdbText -Arguments @('-s', $serial, 'shell', 'getprop', 'ro.product.cpu.abilist')
if ($abis -notmatch 'arm64-v8a') {
    throw "the attached physical device is not arm64"
}

$reverse = Invoke-AdbText -Arguments @('-s', $serial, 'reverse', '--list')
if (-not [string]::IsNullOrWhiteSpace($reverse)) {
    throw "adb reverse is forbidden for the T-110 public-cellular gate"
}

$wifi = Invoke-AdbText -Arguments @('-s', $serial, 'shell', 'cmd', 'wifi', 'status')
if ($wifi -notmatch '(?i)disabled') {
    throw "Wi-Fi must be disabled before the T-110 cellular run"
}

$connectivity = Invoke-AdbText -Arguments @('-s', $serial, 'shell', 'dumpsys', 'connectivity')
if ($connectivity -notmatch 'TRANSPORT_CELLULAR') {
    throw "no active cellular transport was observed"
}
if ($connectivity -match 'TRANSPORT_VPN') {
    throw "VPN transport is forbidden for the T-110 public-cellular gate"
}

$commit = (& git rev-parse HEAD 2>&1 | Select-Object -First 1).Trim()
if ($LASTEXITCODE -ne 0 -or $commit -notmatch '^[0-9a-f]{40}$') {
    throw "the tested repository commit could not be resolved"
}
$dirty = & git status --porcelain
if ($LASTEXITCODE -ne 0 -or $dirty) {
    throw "commit the exact test candidate before collecting T-110 evidence"
}

[ordered]@{
    gate = 'T-110-physical-preflight'
    result = 'pass'
    commit = $commit
    adb_version_observed = -not [string]::IsNullOrWhiteSpace($adbVersion)
    device_class = 'physical-arm64'
    network = 'cellular-no-vpn'
    wifi = 'disabled'
    adb_reverse = 'absent'
} | ConvertTo-Json
