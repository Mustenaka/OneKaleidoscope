[CmdletBinding()]
param(
    [string]$BuildRoot = $env:KALEIDO_BUILD_ROOT,
    [ValidateSet('Audit', 'Clean')]
    [string]$Mode = 'Audit',
    [switch]$Execute,
    [ValidateRange(1, 4096)]
    [int]$WarnUsageGiB = 120,
    [ValidateRange(1, 4096)]
    [int]$MinFreeGiB = 80
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Resolve-ExternalBuildRoot {
    param([Parameter(Mandatory = $true)][string]$Candidate)

    if (-not [System.IO.Path]::IsPathRooted($Candidate) -or
        [System.IO.Path]::GetPathRoot($Candidate) -ne 'D:\') {
        throw 'BuildRoot must be an absolute path on D:.'
    }

    $absolute = [System.IO.Path]::GetFullPath($Candidate).TrimEnd('\')
    if (Test-Path -LiteralPath $absolute) {
        $item = Get-Item -LiteralPath $absolute -Force
        if (-not $item.PSIsContainer) {
            throw 'BuildRoot must name a directory.'
        }
        if ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
            throw 'BuildRoot must not be a reparse point.'
        }
    }

    return $absolute
}

function Get-DirectorySizeBytes {
    param([Parameter(Mandatory = $true)][string]$Path)

    if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
        return [Int64]0
    }

    return [Int64](Get-ChildItem -LiteralPath $Path -Force -Recurse -File -ErrorAction Stop |
        Measure-Object -Property Length -Sum).Sum
}

function Test-ApprovedArtifactTarget {
    param(
        [Parameter(Mandatory = $true)][string]$Root,
        [Parameter(Mandatory = $true)][string]$Candidate
    )

    $rootItem = Get-Item -LiteralPath $Root -Force
    if ($rootItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
        throw 'Resolved build root must not be a reparse point.'
    }
    $candidateItem = Get-Item -LiteralPath $Candidate -Force
    if (-not $candidateItem.PSIsContainer) {
        throw 'Cleanup target must be a directory.'
    }
    if ($candidateItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
        throw 'Cleanup target must not be a reparse point.'
    }

    $resolvedRoot = $rootItem.FullName.TrimEnd('\')
    $resolvedCandidate = $candidateItem.FullName.TrimEnd('\')
    $allowedNames = @('cargo-target', 'gradle-user-home')
    $expected = Join-Path $resolvedRoot $candidateItem.Name
    if ($candidateItem.Name -notin $allowedNames -or
        -not [string]::Equals($resolvedCandidate, $expected, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw 'Cleanup target is outside the approved external artifact directories.'
    }

    return $resolvedCandidate
}

function Test-ArtifactTreeHasNoReparsePoints {
    param([Parameter(Mandatory = $true)][string]$Path)

    $reparsePoint = Get-ChildItem -LiteralPath $Path -Force -Recurse -Attributes ReparsePoint |
        Select-Object -First 1
    if ($null -ne $reparsePoint) {
        throw 'Cleanup target contains a reparse point and cannot be deleted.'
    }
}

if ([string]::IsNullOrWhiteSpace($BuildRoot)) {
    $BuildRoot = 'D:\OneKaleidoscope\build'
}

$resolvedRoot = Resolve-ExternalBuildRoot -Candidate $BuildRoot
$artifacts = @(
    [PSCustomObject]@{ Name = 'cargo-target'; Path = (Join-Path $resolvedRoot 'cargo-target') },
    [PSCustomObject]@{ Name = 'gradle-user-home'; Path = (Join-Path $resolvedRoot 'gradle-user-home') }
)

$totalBytes = [Int64]0
foreach ($artifact in $artifacts) {
    $size = Get-DirectorySizeBytes -Path $artifact.Path
    $artifact | Add-Member -NotePropertyName Bytes -NotePropertyValue $size
    $totalBytes += $size
    $sizeGiB = [Math]::Round($size / 1GB, 2)
    Write-Output ("{0}: {1} GiB" -f $artifact.Name, $sizeGiB)
}

$drive = Get-PSDrive -Name D
$freeGiB = [Math]::Round($drive.Free / 1GB, 2)
$totalGiB = [Math]::Round($totalBytes / 1GB, 2)
Write-Output ("external artifacts total: {0} GiB; D: free: {1} GiB" -f $totalGiB, $freeGiB)
if ($totalGiB -ge $WarnUsageGiB) {
    Write-Warning ("artifact usage meets the {0} GiB warning threshold" -f $WarnUsageGiB)
}
if ($freeGiB -lt $MinFreeGiB) {
    Write-Warning ("D: free space is below the {0} GiB safety threshold" -f $MinFreeGiB)
}

if ($Mode -eq 'Clean') {
    if (-not $Execute) {
        Write-Output 'Dry run only: no artifact directory was deleted. Re-run with -Mode Clean -Execute after explicit user authorization.'
        exit 0
    }

    foreach ($artifact in $artifacts) {
        if (-not (Test-Path -LiteralPath $artifact.Path -PathType Container)) {
            continue
        }
        $approvedTarget = Test-ApprovedArtifactTarget -Root $resolvedRoot -Candidate $artifact.Path
        Test-ArtifactTreeHasNoReparsePoints -Path $approvedTarget
        Remove-Item -LiteralPath $approvedTarget -Recurse -Force -ErrorAction Stop
        Write-Output ("removed approved external artifact directory: {0}" -f $artifact.Name)
    }
}
