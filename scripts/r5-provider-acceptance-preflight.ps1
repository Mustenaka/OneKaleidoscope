param(
    [string]$CodexExecutable,
    [string]$OpenCodeExecutable,
    [string]$ClaudeExecutable,
    [string]$NodeExecutable,
    [switch]$ProviderOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$sdkRoot = if ($env:ANDROID_SDK_ROOT) {
    $env:ANDROID_SDK_ROOT
} elseif ($env:ANDROID_HOME) {
    $env:ANDROID_HOME
} else {
    Join-Path $env:LOCALAPPDATA 'Android\Sdk'
}
$adb = Join-Path $sdkRoot 'platform-tools\adb.exe'
$blockers = [System.Collections.Generic.List[string]]::new()

function Add-Blocker {
    param([Parameter(Mandatory)][string]$Code)

    if (-not $script:blockers.Contains($Code)) {
        $script:blockers.Add($Code)
    }
}

function Resolve-Tool {
    param(
        [string]$ExplicitPath,
        [Parameter(Mandatory)][string]$CommandName,
        [Parameter(Mandatory)][string]$MissingCode
    )

    if ($ExplicitPath) {
        if (Test-Path -LiteralPath $ExplicitPath -PathType Leaf) {
            return (Resolve-Path -LiteralPath $ExplicitPath).Path
        }
        Add-Blocker $MissingCode
        return $null
    }
    $command = Get-Command $CommandName -ErrorAction SilentlyContinue
    if (-not $command -or -not $command.Source) {
        Add-Blocker $MissingCode
        return $null
    }
    return $command.Source
}

function Invoke-Text {
    param(
        [Parameter(Mandatory)][string]$Executable,
        [Parameter(Mandatory)][string[]]$Arguments
    )

    $previousPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        $output = @(& $Executable @Arguments 2>&1)
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousPreference
    }
    if ($exitCode -ne 0) {
        return $null
    }
    return ($output -join "`n").Trim()
}

function Read-PinnedOpenCodeVersion {
    $versions = Get-Content -Encoding utf8 -Raw (Join-Path $repoRoot 'schemas\VERSIONS.md')
    $match = [regex]::Match($versions, '(?s)## OpenCode.*?- CLI: `([^`]+)`')
    if (-not $match.Success) {
        Add-Blocker 'opencode_schema_version_missing'
        return $null
    }
    return $match.Groups[1].Value
}

function Read-PinnedClaudeSdkVersion {
    $packagePath = Join-Path $repoRoot 'crates\kaleido-adapter-claude\bridge\package.json'
    try {
        $package = Get-Content -Encoding utf8 -Raw $packagePath | ConvertFrom-Json
        return $package.dependencies.'@anthropic-ai/claude-agent-sdk'
    } catch {
        Add-Blocker 'claude_sdk_version_missing'
        return $null
    }
}

Push-Location $repoRoot
try {
    $commitOutput = @(& git rev-parse HEAD 2>$null)
    $commitExitCode = $LASTEXITCODE
    $commit = if ($commitOutput.Count -gt 0) { $commitOutput[0].Trim() } else { '' }
    if ($commitExitCode -ne 0 -or $commit -notmatch '^[0-9a-f]{40}$') {
        Add-Blocker 'git_commit_unavailable'
    }
    & git merge-base --is-ancestor origin/main HEAD 2>$null
    if ($LASTEXITCODE -ne 0) {
        Add-Blocker 'candidate_does_not_include_origin_main'
    }
    $dirty = @(& git status --porcelain 2>$null)
    if ($LASTEXITCODE -ne 0 -or $dirty.Count -ne 0) {
        Add-Blocker 'working_tree_dirty'
    }

    $codex = Resolve-Tool -ExplicitPath $CodexExecutable -CommandName 'codex.exe' -MissingCode 'native_codex_missing'
    $codexVersion = $null
    if ($codex) {
        $codexVersion = Invoke-Text -Executable $codex -Arguments @('--version')
        if (-not $codexVersion -or $codexVersion -notmatch '^codex-cli\s+\d') {
            Add-Blocker 'native_codex_unusable'
        }
    }

    $opencode = Resolve-Tool -ExplicitPath $OpenCodeExecutable -CommandName 'opencode' -MissingCode 'opencode_missing'
    $opencodeVersion = $null
    $pinnedOpenCode = Read-PinnedOpenCodeVersion
    if ($opencode) {
        $opencodeVersion = Invoke-Text -Executable $opencode -Arguments @('--version')
        if (-not $opencodeVersion) {
            Add-Blocker 'opencode_unusable'
        } elseif ($pinnedOpenCode -and $opencodeVersion -ne $pinnedOpenCode) {
            Add-Blocker 'opencode_version_mismatch'
        }
    }

    $claude = Resolve-Tool -ExplicitPath $ClaudeExecutable -CommandName 'claude' -MissingCode 'claude_cli_missing'
    $claudeLoggedIn = $false
    $claudeAuthMethod = $null
    if ($claude) {
        $authText = Invoke-Text -Executable $claude -Arguments @('auth', 'status', '--json')
        if ($authText) {
            try {
                $auth = $authText | ConvertFrom-Json
                $claudeLoggedIn = $auth.loggedIn -eq $true
                $claudeAuthMethod = [string]$auth.authMethod
            } catch {
                Add-Blocker 'claude_auth_status_invalid'
            }
        }
        if (-not $claudeLoggedIn) {
            Add-Blocker 'claude_auth_missing'
        }
    }

    $node = Resolve-Tool -ExplicitPath $NodeExecutable -CommandName 'node' -MissingCode 'node_missing'
    $nodeVersion = $null
    if ($node) {
        $nodeVersion = Invoke-Text -Executable $node -Arguments @('--version')
        if (-not $nodeVersion -or $nodeVersion -notmatch '^v(2[2-9]|[3-9]\d)\.') {
            Add-Blocker 'node_22_or_newer_required'
        }
    }
    $claudeSdkVersion = Read-PinnedClaudeSdkVersion

    $deviceClass = if ($ProviderOnly) { 'not-requested' } else { 'unavailable' }
    $deviceAbi = $null
    $network = if ($ProviderOnly) { 'not-requested' } else { 'unavailable' }
    if (-not $ProviderOnly) {
        if (-not (Test-Path -LiteralPath $adb -PathType Leaf)) {
            Add-Blocker 'adb_missing'
        } else {
            $deviceText = Invoke-Text -Executable $adb -Arguments @('devices')
            $devices = @(
                if ($deviceText) {
                    $deviceText -split "`n" |
                        Where-Object { $_ -match '^([^\s]+)\s+device$' } |
                        ForEach-Object { $Matches[1] }
                }
            )
            if ($devices.Count -ne 1) {
                Add-Blocker 'exactly_one_android_device_required'
            } else {
                $serial = $devices[0]
                $qemu = Invoke-Text -Executable $adb -Arguments @('-s', $serial, 'shell', 'getprop', 'ro.kernel.qemu')
                $deviceAbi = Invoke-Text -Executable $adb -Arguments @('-s', $serial, 'shell', 'getprop', 'ro.product.cpu.abilist')
                if ($qemu -eq '1' -or $serial.StartsWith('emulator-')) {
                    Add-Blocker 'physical_android_required'
                    $deviceClass = 'emulator'
                } elseif (-not $deviceAbi -or $deviceAbi -notmatch '(^|,)arm64-v8a(,|$)') {
                    Add-Blocker 'arm64_android_required'
                    $deviceClass = 'physical-non-arm64'
                } else {
                    $deviceClass = 'physical-arm64'
                }
                $reverse = Invoke-Text -Executable $adb -Arguments @('-s', $serial, 'reverse', '--list')
                if (-not [string]::IsNullOrWhiteSpace($reverse)) {
                    Add-Blocker 'adb_reverse_forbidden'
                }
                $connectivity = Invoke-Text -Executable $adb -Arguments @('-s', $serial, 'shell', 'dumpsys', 'connectivity')
                if (-not $connectivity -or $connectivity -notmatch 'TRANSPORT_WIFI') {
                    Add-Blocker 'wifi_transport_required'
                } elseif ($connectivity -match 'TRANSPORT_VPN') {
                    Add-Blocker 'vpn_transport_forbidden'
                } else {
                    $network = 'wifi-no-vpn'
                }
            }
        }
    }

    $result = if ($blockers.Count -eq 0) { 'pass' } else { 'blocked' }
    [ordered]@{
        gate = 'R5-provider-acceptance-preflight'
        result = $result
        commit = $commit
        codex_version = $codexVersion
        opencode_version = $opencodeVersion
        opencode_schema_version = $pinnedOpenCode
        claude_logged_in = $claudeLoggedIn
        claude_auth_method = $claudeAuthMethod
        claude_sdk_version = $claudeSdkVersion
        node_version = $nodeVersion
        android_device_class = $deviceClass
        android_abi_observed = -not [string]::IsNullOrWhiteSpace($deviceAbi)
        android_network = $network
        blockers = @($blockers)
    } | ConvertTo-Json -Depth 4
    if ($blockers.Count -ne 0) {
        exit 1
    }
} finally {
    Pop-Location
}
