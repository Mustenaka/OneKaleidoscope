[CmdletBinding()]
param(
    [string]$BuildRoot = $env:KALEIDO_BUILD_ROOT
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Resolve-ExternalBuildRoot {
    param([Parameter(Mandatory = $true)][string]$Candidate)

    $root = [System.IO.Path]::GetPathRoot($Candidate)
    if (-not [System.IO.Path]::IsPathRooted($Candidate) -or
        $root -notmatch '^[A-Za-z]:\\$') {
        throw 'KALEIDO_BUILD_ROOT must be an absolute path on a local Windows drive.'
    }

    $absolute = [System.IO.Path]::GetFullPath($Candidate)

    if (Test-Path -LiteralPath $absolute) {
        $item = Get-Item -LiteralPath $absolute -Force
        if (-not $item.PSIsContainer) {
            throw 'KALEIDO_BUILD_ROOT must name a directory.'
        }
        if ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
            throw 'KALEIDO_BUILD_ROOT must not be a reparse point.'
        }
    }

    return $absolute.TrimEnd('\')
}

if ([string]::IsNullOrWhiteSpace($BuildRoot)) {
    if ($null -ne (Get-PSDrive -Name D -ErrorAction SilentlyContinue)) {
        $BuildRoot = 'D:\OneKaleidoscope\build'
    } else {
        $BuildRoot = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'OneKaleidoscope\build'
    }
}

$resolvedRoot = Resolve-ExternalBuildRoot -Candidate $BuildRoot
[System.IO.Directory]::CreateDirectory($resolvedRoot) | Out-Null
$createdRoot = Get-Item -LiteralPath $resolvedRoot -Force
if ($createdRoot.Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
    throw 'KALEIDO_BUILD_ROOT must not be a reparse point.'
}

$env:KALEIDO_BUILD_ROOT = $resolvedRoot
$env:CARGO_TARGET_DIR = Join-Path $resolvedRoot 'cargo-target'
$env:GRADLE_USER_HOME = Join-Path $resolvedRoot 'gradle-user-home'

Write-Output 'Configured an external build root for this PowerShell session.'
Write-Output 'Cargo and Gradle artifacts will use the shared external directories.'
