param(
    [string]$BindAddress,
    [string]$CodexExecutable,
    [int]$BackgroundSeconds = 90
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$androidRoot = Join-Path $repoRoot 'apps\android'
$sdkRoot = if ($env:ANDROID_SDK_ROOT) { $env:ANDROID_SDK_ROOT } else { Join-Path $env:LOCALAPPDATA 'Android\Sdk' }
$adb = Join-Path $sdkRoot 'platform-tools\adb.exe'
$gradle = Join-Path $androidRoot 'gradlew.bat'

if (-not (Test-Path -LiteralPath $adb -PathType Leaf)) { throw 'Android platform-tools adb.exe is unavailable' }
if (-not (Test-Path -LiteralPath $gradle -PathType Leaf)) { throw 'Android Gradle wrapper is unavailable' }
if ($BackgroundSeconds -lt 60) { throw 'BackgroundSeconds must be at least 60' }
$env:ANDROID_HOME = $sdkRoot
$env:ANDROID_SDK_ROOT = $sdkRoot

function Invoke-Adb {
    param([string[]]$AdbArguments, [string]$Description)
    $output = @(& $adb @AdbArguments 2>&1)
    if ($LASTEXITCODE -ne 0) { throw "ADB failed during $Description" }
    return $output
}

function Read-HostLine {
    param([System.Diagnostics.Process]$Process, [string]$Description)
    $task = $Process.StandardOutput.ReadLineAsync()
    if (-not $task.Wait([TimeSpan]::FromSeconds(60))) { throw "host timed out during $Description" }
    $line = $task.Result
    if ([string]::IsNullOrWhiteSpace($line)) { throw "host returned an empty response during $Description" }
    return $line
}

function Run-Instrumentation {
    param([string[]]$Arguments, [string]$Description)
    $output = Invoke-Adb -Description $Description -AdbArguments (@(
        '-s', $script:deviceSerial, 'shell', 'am', 'instrument', '-w', '-r'
    ) + $Arguments + @('com.onekaleidoscope.test/androidx.test.runner.AndroidJUnitRunner'))
    $joined = $output -join "`n"
    if ($joined -notmatch 'OK \(1 test\)' -or $joined -match 'FAILURES!!!|INSTRUMENTATION_FAILED') {
        $safeDiagnostic = $output | Where-Object {
            $_ -match '^INSTRUMENTATION_STATUS: (class|test|stack)=' -or
                $_ -match '^INSTRUMENTATION_STATUS_CODE:' -or
                $_ -match '^INSTRUMENTATION_(RESULT: shortMsg|CODE:)' -or
                $_ -match '^FAILURES!!!$'
        }
        throw "instrumentation did not pass during $Description`n$($safeDiagnostic -join "`n")"
    }
    return $output
}

function Run-LanPhase {
    param([string]$Phase, [string]$PairingUri, [bool]$RequireAttention = $false)
    $arguments = @('-e', 'class', 'com.onekaleidoscope.integration.RealLanBridgeTest', '-e', 'lanPhase', $Phase)
    if ($PairingUri) { $arguments += @('-e', 'pairingUri', $PairingUri) }
    if ($RequireAttention) { $arguments += @('-e', 'requireAttention', 'true') }
    return Run-Instrumentation -Arguments $arguments -Description "real LAN phase $Phase"
}

function Get-EvidenceValue {
    param([string[]]$Output, [string]$Name)
    $prefix = "INSTRUMENTATION_STATUS: onekaleidoscope.$Name="
    $line = $Output | Where-Object { $_.StartsWith($prefix, [StringComparison]::Ordinal) } | Select-Object -Last 1
    if (-not $line) { throw "instrumentation omitted required $Name evidence" }
    return $line.Substring($prefix.Length)
}

function Resolve-NativeCodexExecutable {
    param([string]$ExplicitPath)

    $candidates = [System.Collections.Generic.List[string]]::new()
    if ($ExplicitPath) { $candidates.Add($ExplicitPath) }
    if ($env:CODEX_EXECUTABLE) { $candidates.Add($env:CODEX_EXECUTABLE) }

    $commands = @(Get-Command codex.exe -CommandType Application -All -ErrorAction SilentlyContinue)
    foreach ($command in $commands) { $candidates.Add($command.Source) }

    $launchers = @(Get-Command codex.cmd -CommandType Application -All -ErrorAction SilentlyContinue)
    foreach ($launcher in $launchers) {
        $launcherDirectory = Split-Path -Parent $launcher.Source
        $vendorRoot = Join-Path $launcherDirectory 'node_modules\@openai\codex\node_modules'
        if (Test-Path -LiteralPath $vendorRoot -PathType Container) {
            Get-ChildItem -LiteralPath $vendorRoot -Recurse -Filter codex.exe -File -ErrorAction SilentlyContinue |
                ForEach-Object { $candidates.Add($_.FullName) }
        }
    }

    if (Get-Command Get-AppxPackage -ErrorAction SilentlyContinue) {
        $packages = @(Get-AppxPackage -Name OpenAI.Codex -ErrorAction SilentlyContinue)
        foreach ($package in $packages) {
            $candidates.Add((Join-Path $package.InstallLocation 'app\resources\codex.exe'))
        }
    }

    foreach ($candidate in ($candidates | Select-Object -Unique)) {
        if (-not $candidate -or -not (Test-Path -LiteralPath $candidate -PathType Leaf)) { continue }
        try {
            $version = @(& $candidate --version 2>$null)
            if ($LASTEXITCODE -eq 0 -and ($version -join ' ') -match '^codex-cli\s+\d+') {
                return (Resolve-Path -LiteralPath $candidate).Path
            }
        } catch {
            continue
        }
    }

    throw 'a runnable native codex.exe is required; pass -CodexExecutable explicitly or set CODEX_EXECUTABLE'
}

function ConvertTo-WindowsCommandLineArgument {
    param([Parameter(Mandatory)][string]$Argument)

    if ($Argument.Length -gt 0 -and $Argument -notmatch '[\s"]') { return $Argument }
    $escaped = $Argument -replace '(\\*)"', '$1$1\"'
    $escaped = $escaped -replace '(\\+)$', '$1$1'
    return '"' + $escaped + '"'
}

$deviceLines = @(& $adb devices)
if ($LASTEXITCODE -ne 0) { throw 'adb devices failed' }
$devices = @($deviceLines | Where-Object { $_ -match '^([^\s]+)\s+device$' } | ForEach-Object { $Matches[1] })
if ($devices.Count -ne 1) { throw "physical gate requires exactly one authorized Android device; found $($devices.Count)" }
$script:deviceSerial = $devices[0]

$qemu = (Invoke-Adb -AdbArguments @('-s', $deviceSerial, 'shell', 'getprop', 'ro.kernel.qemu') -Description 'emulator check' | Select-Object -First 1).Trim()
$abiList = (Invoke-Adb -AdbArguments @('-s', $deviceSerial, 'shell', 'getprop', 'ro.product.cpu.abilist') -Description 'ABI check' | Select-Object -First 1).Trim()
if ($qemu -eq '1') { throw 'the selected device is an emulator' }
if ($abiList.Split(',') -notcontains 'arm64-v8a') { throw 'the selected physical device is not arm64-v8a capable' }

if (-not $BindAddress) {
    $BindAddress = [System.Net.Dns]::GetHostAddresses([System.Net.Dns]::GetHostName()) |
        Where-Object { $_.AddressFamily -eq [System.Net.Sockets.AddressFamily]::InterNetwork -and -not [System.Net.IPAddress]::IsLoopback($_) } |
        Select-Object -First 1 -ExpandProperty IPAddressToString
}
if (-not $BindAddress) { throw 'no reachable PC LAN IPv4 address was found; pass -BindAddress explicitly' }
$bindEndpoint = "${BindAddress}:0"

$CodexExecutable = Resolve-NativeCodexExecutable -ExplicitPath $CodexExecutable

Push-Location $repoRoot
$hostProcess = $null
$idleForced = $false
$gateStamp = [DateTimeOffset]::UtcNow.ToString('yyyyMMddTHHmmssZ')
$gateRoot = Join-Path $repoRoot "target\physical-arm64-gate\$gateStamp"
$dataRoot = Join-Path $gateRoot 'host-data'
$projectRoot = Join-Path $gateRoot 'approval-project'
$approvalProbe = Join-Path $projectRoot 'editable.txt'
$evidencePath = Join-Path $gateRoot 'evidence.json'
New-Item -ItemType Directory -Force -Path $dataRoot | Out-Null
New-Item -ItemType Directory -Force -Path $projectRoot | Out-Null
[IO.File]::WriteAllText($approvalProbe, 'ORIGINAL', [Text.UTF8Encoding]::new($false))

try {
    & cargo run -p xtask -- ci
    if ($LASTEXITCODE -ne 0) { throw 'cargo xtask ci failed' }
    & cargo build -p kaleido-hostd --example physical_gate_host
    if ($LASTEXITCODE -ne 0) { throw 'physical gate host build failed' }
    & $gradle -p $androidRoot --no-daemon --console=plain :app:assembleDebug :app:assembleDebugAndroidTest
    if ($LASTEXITCODE -ne 0) { throw 'Android APK build failed' }

    $appApk = Join-Path $repoRoot 'apps\android\app\build\outputs\apk\debug\app-debug.apk'
    $testApk = Join-Path $repoRoot 'apps\android\app\build\outputs\apk\androidTest\debug\app-debug-androidTest.apk'
    Invoke-Adb -AdbArguments @('-s', $deviceSerial, 'install', '-r', '-t', $appApk) -Description 'app installation' | Out-Null
    Invoke-Adb -AdbArguments @('-s', $deviceSerial, 'install', '-r', '-t', $testApk) -Description 'test installation' | Out-Null

    Run-Instrumentation -Description 'hardware-backed AndroidKeyStore gate' -Arguments @(
        '-e', 'class', 'com.onekaleidoscope.platform.PhysicalDeviceSecurityGateTest',
        '-e', 'requireHardwareBacked', 'true'
    ) | Out-Null

    $hostExecutable = Join-Path $repoRoot 'target\debug\examples\physical_gate_host.exe'
    $start = [System.Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $hostExecutable
    $start.UseShellExecute = $false
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $hostArguments = @(
        '--executable', $CodexExecutable,
        '--project-root', $projectRoot,
        '--data-dir', $dataRoot,
        '--bind', $bindEndpoint,
        '--sandbox', 'read-only'
    ) | ForEach-Object { ConvertTo-WindowsCommandLineArgument -Argument $_ }
    $start.Arguments = $hostArguments -join ' '
    $hostProcess = [System.Diagnostics.Process]::new()
    $hostProcess.StartInfo = $start
    if (-not $hostProcess.Start()) { throw 'physical gate host did not start' }
    $stderrDrain = $hostProcess.StandardError.ReadToEndAsync()
    $pairingUri = Read-HostLine -Process $hostProcess -Description 'initial pairing issue'
    if (-not $pairingUri.StartsWith('onekaleidoscope://pair/v1?data=', [StringComparison]::Ordinal)) {
        throw 'host returned an invalid pairing URI'
    }

    Invoke-Adb -AdbArguments @('-s', $deviceSerial, 'shell', 'pm', 'clear', 'com.onekaleidoscope') -Description 'wrong-pin reset' | Out-Null
    Run-LanPhase -Phase 'wrong-pin' -PairingUri $pairingUri | Out-Null

    $hostProcess.StandardInput.WriteLine('pair')
    $hostProcess.StandardInput.Flush()
    $pairingUri = Read-HostLine -Process $hostProcess -Description 'fresh pairing issue'
    Invoke-Adb -AdbArguments @('-s', $deviceSerial, 'shell', 'pm', 'clear', 'com.onekaleidoscope') -Description 'seed reset' | Out-Null
    $seedOutput = Run-LanPhase -Phase 'seed' -PairingUri $pairingUri -RequireAttention $true
    $deviceId = Get-EvidenceValue -Output $seedOutput -Name 'deviceId'
    $seedCursor = Get-EvidenceValue -Output $seedOutput -Name 'cursor'
    $seedOutcome = Get-EvidenceValue -Output $seedOutput -Name 'outcome'
    if ($seedOutcome -notmatch '^seed-seven-projections-(submit-prompt|enqueue-new-turn)-attention-declined$') {
        throw 'seed phase did not prove a real Android attention response'
    }
    if ([IO.File]::ReadAllText($approvalProbe) -ne 'ORIGINAL') {
        throw 'declined file-change approval modified the isolated probe file'
    }

    Invoke-Adb -AdbArguments @('-s', $deviceSerial, 'shell', 'am', 'start', '-W', '-n', 'com.onekaleidoscope/.MainActivity') -Description 'foreground launch' | Out-Null
    Start-Sleep -Seconds 5
    $pidBefore = (Invoke-Adb -AdbArguments @('-s', $deviceSerial, 'shell', 'pidof', 'com.onekaleidoscope') -Description 'background PID before' | Select-Object -First 1).Trim()
    Invoke-Adb -AdbArguments @('-s', $deviceSerial, 'shell', 'input', 'keyevent', '3') -Description 'send app to background' | Out-Null
    $idleOutput = @(& $adb -s $deviceSerial shell dumpsys deviceidle force-idle 2>&1)
    if ($LASTEXITCODE -eq 0 -and ($idleOutput -join ' ') -match 'Now forced in to idle mode|Stepped to deep') { $idleForced = $true }
    Start-Sleep -Seconds $BackgroundSeconds
    if ($idleForced) { Invoke-Adb -AdbArguments @('-s', $deviceSerial, 'shell', 'dumpsys', 'deviceidle', 'unforce') -Description 'leave device idle' | Out-Null }
    $pidAfter = [string](@(& $adb -s $deviceSerial shell pidof com.onekaleidoscope 2>$null | Select-Object -First 1) -join '')
    $backgroundOutput = Run-LanPhase -Phase 'background' -PairingUri ''
    if ((Get-EvidenceValue -Output $backgroundOutput -Name 'outcome') -ne 'oem-background-resumed') {
        throw 'OEM background phase omitted its reconnect evidence'
    }
    $backgroundCursor = Get-EvidenceValue -Output $backgroundOutput -Name 'cursor'

    Invoke-Adb -AdbArguments @('-s', $deviceSerial, 'shell', 'am', 'force-stop', 'com.onekaleidoscope') -Description 'external force-stop' | Out-Null
    Start-Sleep -Seconds 2
    $resumeOutput = Run-LanPhase -Phase 'resume' -PairingUri ''
    if ((Get-EvidenceValue -Output $resumeOutput -Name 'outcome') -ne 'force-stop-cache-cursor-resumed') {
        throw 'force-stop phase omitted exact resume evidence'
    }
    $resumeCursor = Get-EvidenceValue -Output $resumeOutput -Name 'cursor'
    $resumeFromCursor = Get-EvidenceValue -Output $resumeOutput -Name 'resumeFromCursor'
    if ($backgroundCursor -ne $resumeFromCursor) {
        throw 'force-stop cold start did not load the last background ProjectIndex cursor'
    }
    if ([UInt64]$resumeCursor -lt [UInt64]$resumeFromCursor) {
        throw 'ProjectIndex cursor moved backwards during cold resume'
    }

    $hostProcess.StandardInput.WriteLine("revoke $deviceId")
    $hostProcess.StandardInput.Flush()
    if ((Read-HostLine -Process $hostProcess -Description 'durable device revoke') -ne 'REVOKED') {
        throw 'host did not durably revoke the paired device'
    }
    Invoke-Adb -AdbArguments @('-s', $deviceSerial, 'shell', 'am', 'force-stop', 'com.onekaleidoscope') -Description 'post-revoke force-stop' | Out-Null
    $revokedOutput = Run-LanPhase -Phase 'revoked' -PairingUri ''
    if ((Get-EvidenceValue -Output $revokedOutput -Name 'outcome') -ne 'revoked-authentication') {
        throw 'revoked phase omitted authentication rejection evidence'
    }

    $manufacturer = (Invoke-Adb -AdbArguments @('-s', $deviceSerial, 'shell', 'getprop', 'ro.product.manufacturer') -Description 'manufacturer evidence' | Select-Object -First 1).Trim()
    $model = (Invoke-Adb -AdbArguments @('-s', $deviceSerial, 'shell', 'getprop', 'ro.product.model') -Description 'model evidence' | Select-Object -First 1).Trim()
    $androidSdk = (Invoke-Adb -AdbArguments @('-s', $deviceSerial, 'shell', 'getprop', 'ro.build.version.sdk') -Description 'SDK evidence' | Select-Object -First 1).Trim()
    $commit = (& git rev-parse HEAD).Trim()
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try {
        $deviceIdHash = $sha256.ComputeHash([Text.Encoding]::UTF8.GetBytes($deviceId))
    } finally {
        $sha256.Dispose()
    }
    $deviceIdDigest = ($deviceIdHash | Select-Object -First 8 | ForEach-Object { $_.ToString('x2') }) -join ''
    $evidence = [ordered]@{
        gate = 'R3 physical arm64 Android'
        completed_at_utc = [DateTimeOffset]::UtcNow.ToString('O')
        commit = $commit
        manufacturer = $manufacturer
        model = $model
        android_sdk = $androidSdk
        abi_list = $abiList
        hardware_backed_keystore = $true
        real_wifi_without_adb_reverse = $true
        attention_declined_on_android = $true
        declined_approval_left_probe_unchanged = $true
        prompt_or_enqueue_accepted = $true
        oem_background_seconds = $BackgroundSeconds
        device_idle_forced = $idleForced
        process_survived_background = ($pidBefore -and $pidAfter -and $pidBefore -eq $pidAfter)
        force_stop_cursor_resumed_exactly = $true
        revoked_authentication_rejected = $true
        device_id_sha256_prefix = $deviceIdDigest
        phases = @('hardware', 'wrong-pin', 'seed-attention', 'background', 'force-stop-resume', 'revoked')
    }
    $evidence | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $evidencePath -Encoding UTF8
    Write-Host "R3 physical arm64 gate PASS; evidence: $evidencePath"
} finally {
    if ($idleForced) { & $adb -s $deviceSerial shell dumpsys deviceidle unforce *> $null }
    if ($hostProcess -and -not $hostProcess.HasExited) {
        $hostProcess.StandardInput.WriteLine('stop')
        $hostProcess.StandardInput.Flush()
        if (-not $hostProcess.WaitForExit(30000)) {
            & taskkill.exe /PID $hostProcess.Id /T /F *> $null
            [void]$hostProcess.WaitForExit(30000)
        }
    }
    Pop-Location
}
