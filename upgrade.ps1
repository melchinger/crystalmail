[CmdletBinding()]
param(
    [string]$SourceExe,
    [switch]$Build,
    [string]$BuildCommand = "npm run tauri -- build --bundles app",
    [string]$WorkspaceRoot
)

$ErrorActionPreference = "Stop"

function Get-GitOutput {
    param(
        [string]$WorkingDirectory,
        [string[]]$Arguments
    )

    Push-Location -LiteralPath $WorkingDirectory
    try {
        $output = & git @Arguments 2>$null
        if ($LASTEXITCODE -ne 0) {
            return $null
        }

        return ($output | Select-Object -First 1).Trim()
    }
    finally {
        Pop-Location
    }
}

function Resolve-WorkspaceRoot {
    param(
        [string]$ExplicitWorkspaceRoot,
        [string]$ScriptPath
    )

    if ($ExplicitWorkspaceRoot) {
        return (Resolve-Path -LiteralPath $ExplicitWorkspaceRoot).Path
    }

    $scriptDirectory = Split-Path -Parent $ScriptPath
    $scriptTopLevel = Get-GitOutput -WorkingDirectory $scriptDirectory -Arguments @("rev-parse", "--show-toplevel")
    $scriptCommonDir = Get-GitOutput -WorkingDirectory $scriptDirectory -Arguments @("rev-parse", "--git-common-dir")
    $invocationDirectory = (Get-Location).Path
    $invocationTopLevel = Get-GitOutput -WorkingDirectory $invocationDirectory -Arguments @("rev-parse", "--show-toplevel")
    $invocationCommonDir = Get-GitOutput -WorkingDirectory $invocationDirectory -Arguments @("rev-parse", "--git-common-dir")

    if (
        $scriptTopLevel -and
        $invocationTopLevel -and
        $scriptCommonDir -and
        $invocationCommonDir -and
        [System.StringComparer]::OrdinalIgnoreCase.Equals($scriptCommonDir, $invocationCommonDir)
    ) {
        return $invocationTopLevel
    }

    if ($scriptTopLevel) {
        return $scriptTopLevel
    }

    return $scriptDirectory
}

$repoRoot = Resolve-WorkspaceRoot -ExplicitWorkspaceRoot $WorkspaceRoot -ScriptPath $MyInvocation.MyCommand.Path
$productName = "CrystalMail"
$installedExeName = "crystalmail.exe"
$defaultSourceCandidates = @(
    (Join-Path $repoRoot "src-tauri\target\release\CrystalMail.exe"),
    (Join-Path $repoRoot "target\release\CrystalMail.exe")
)

function Invoke-LocalBuild {
    param(
        [string]$Command,
        [string]$WorkingDirectory
    )

    Write-Host "Build  : $Command"

    Push-Location -LiteralPath $WorkingDirectory
    try {
        Invoke-Expression $Command
        if ($LASTEXITCODE -ne 0) {
            throw "Build fehlgeschlagen mit Exit-Code $LASTEXITCODE."
        }
    }
    finally {
        Pop-Location
    }
}

function Resolve-SourceExe {
    param(
        [string]$ExplicitPath,
        [string[]]$CandidatePaths
    )

    if ($ExplicitPath) {
        $resolved = Resolve-Path -LiteralPath $ExplicitPath
        return $resolved.Path
    }

    foreach ($candidate in $CandidatePaths) {
        if (Test-Path -LiteralPath $candidate) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }

    throw "Keine kompilierte CrystalMail.exe gefunden. Erwarte z.B. 'src-tauri\target\release\CrystalMail.exe' oder nutze -SourceExe."
}

function Get-InstallPathFromRegistry {
    param(
        [string]$Name,
        [string]$ExeName
    )

    $registryRoots = @(
        "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*",
        "HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*",
        "HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*"
    )

    foreach ($root in $registryRoots) {
        $entry = Get-ItemProperty -Path $root -ErrorAction SilentlyContinue |
            Where-Object { $_.DisplayName -eq $Name } |
            Select-Object -First 1

        if (-not $entry) {
            continue
        }

        if ($entry.InstallLocation) {
            $installLocation = $entry.InstallLocation.Trim().Trim('"')
            $candidates = @(
                (Join-Path $installLocation $ExeName),
                (Join-Path $installLocation "$Name.exe")
            )

            foreach ($candidate in $candidates) {
                if (Test-Path -LiteralPath $candidate) {
                    return (Resolve-Path -LiteralPath $candidate).Path
                }
            }
        }

        if ($entry.DisplayIcon) {
            $displayIcon = $entry.DisplayIcon.Trim('"')
            $exeCandidate = ($displayIcon -split ",")[0]
            if (Test-Path -LiteralPath $exeCandidate) {
                return (Resolve-Path -LiteralPath $exeCandidate).Path
            }
        }
    }

    return $null
}

function Resolve-InstalledExe {
    param(
        [string]$Name,
        [string]$ExeName
    )

    $registryPath = Get-InstallPathFromRegistry -Name $Name -ExeName $ExeName
    if ($registryPath) {
        return $registryPath
    }

    $fallbackCandidates = @(
        (Join-Path $env:LOCALAPPDATA "$Name\$ExeName"),
        (Join-Path $env:LOCALAPPDATA "Programs\$Name\$ExeName"),
        (Join-Path $env:ProgramFiles "$Name\$ExeName"),
        (Join-Path ${env:ProgramFiles(x86)} "$Name\$ExeName")
    ) | Where-Object { $_ }

    foreach ($candidate in $fallbackCandidates) {
        if (Test-Path -LiteralPath $candidate) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }

    throw "Keine installierte $Name.exe gefunden. Wenn der Installer an einen unueblichen Ort installiert hat, passe das Skript an."
}

function Stop-InstalledProcess {
    param(
        [string]$InstalledExePath
    )

    $running = Get-Process -Name "CrystalMail" -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and ([System.StringComparer]::OrdinalIgnoreCase.Equals($_.Path, $InstalledExePath)) }

    if ($running) {
        $running | Stop-Process -Force
        Start-Sleep -Seconds 1
    }
}

if ($Build) {
    Invoke-LocalBuild -Command $BuildCommand -WorkingDirectory $repoRoot
}

$sourceExePath = Resolve-SourceExe -ExplicitPath $SourceExe -CandidatePaths $defaultSourceCandidates
$installedExePath = Resolve-InstalledExe -Name $productName -ExeName $installedExeName

$sourceVersion = (Get-Item -LiteralPath $sourceExePath).VersionInfo.FileVersion
$targetVersionBefore = (Get-Item -LiteralPath $installedExePath).VersionInfo.FileVersion

Write-Host "Quelle : $sourceExePath ($sourceVersion)"
Write-Host "Ziel   : $installedExePath ($targetVersionBefore)"

Stop-InstalledProcess -InstalledExePath $installedExePath

$installedDirectory = Split-Path -Parent $installedExePath
if (-not (Test-Path -LiteralPath $installedDirectory)) {
    throw "Installationsverzeichnis fehlt: $installedDirectory"
}

Copy-Item -LiteralPath $sourceExePath -Destination $installedExePath -Force

$targetVersionAfter = (Get-Item -LiteralPath $installedExePath).VersionInfo.FileVersion
Write-Host "Upgrade abgeschlossen. Installierte Version: $targetVersionAfter"
