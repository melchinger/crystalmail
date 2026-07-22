[CmdletBinding()]
param(
    [string]$SourceExe,
    [switch]$Build,
    # Nur die exe bauen (kein msi/nsis-Installer) - das Skript kopiert
    # sowieso die rohe exe aus target/release, das Bundling waere
    # verschwendete Zeit.
    [string]$BuildCommand = "npm run tauri -- build --no-bundle",
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

# Soll-Version: tauri.conf.json ist die Source of Truth - sie treibt
# Bundle-Metadaten und (via tauri-build) die FileVersion der exe.
# Damit nie ein veralteter Build aus target/release installiert wird,
# gleichen wir die gefundene Quell-exe dagegen und bauen bei
# Abweichung automatisch neu.
function Get-ExpectedVersion {
    param([string]$Root)

    $confPath = Join-Path $Root "src-tauri\tauri.conf.json"
    $conf = Get-Content -LiteralPath $confPath -Raw | ConvertFrom-Json
    if (-not $conf.version) {
        throw "Keine 'version' in $confPath gefunden."
    }
    return $conf.version.Trim()
}

# exe-FileVersion ist vierstellig ("0.6.0.0"), die Repo-Version
# dreistellig ("0.6.0") - fuer den Vergleich normalisieren.
function ConvertTo-NormalizedVersion {
    param([string]$Version)

    if (-not $Version) { return $null }
    $v = $Version.Trim()
    if ($v -match '^(\d+\.\d+\.\d+)\.0$') { return $Matches[1] }
    return $v
}
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

$expectedVersion = Get-ExpectedVersion -Root $repoRoot

# Quell-exe suchen (bei -SourceExe explizit, sonst die Standardpfade).
# Fehlt sie, wird weiter unten gebaut.
$sourceExePath = $null
$sourceVersion = $null
# Vorsicht mit PowerShell-Praezedenz: `-or` bindet STAERKER als die
# Pipeline, `$a -or $b | Where {...}` wuerde als `($a -or $b) | Where {...}`
# geparst und der Kandidaten-Check liefe ins Leere. Darum die
# Existenzpruefung explizit in eine eigene Variable ziehen.
$existingCandidates = @($defaultSourceCandidates | Where-Object { Test-Path -LiteralPath $_ })
if ($SourceExe -or $existingCandidates.Count -gt 0) {
    $sourceExePath = Resolve-SourceExe -ExplicitPath $SourceExe -CandidatePaths $defaultSourceCandidates
    $sourceVersion = ConvertTo-NormalizedVersion -Version (Get-Item -LiteralPath $sourceExePath).VersionInfo.FileVersion
}

# Build-Entscheidung: explizit erzwungen (-Build), keine exe da, oder
# die gefundene exe passt nicht zur Repo-Version (veralteter Build).
$needsBuild = $Build -or (-not $sourceExePath) -or ($sourceVersion -ne $expectedVersion)
if ($needsBuild -and -not $Build) {
    if (-not $sourceExePath) {
        Write-Host "Keine kompilierte exe gefunden - baue Version $expectedVersion."
    } else {
        Write-Host "Gefundene exe ist $sourceVersion, Repo ist $expectedVersion - baue neu."
    }
}
if ($needsBuild) {
    Invoke-LocalBuild -Command $BuildCommand -WorkingDirectory $repoRoot
    $sourceExePath = Resolve-SourceExe -ExplicitPath $SourceExe -CandidatePaths $defaultSourceCandidates
    $sourceVersion = ConvertTo-NormalizedVersion -Version (Get-Item -LiteralPath $sourceExePath).VersionInfo.FileVersion
    if ($sourceVersion -ne $expectedVersion) {
        throw "Build lieferte $sourceVersion, erwartet war $expectedVersion. Laufen package.json / tauri.conf.json / Cargo.toml auseinander?"
    }
}

$installedExePath = Resolve-InstalledExe -Name $productName -ExeName $installedExeName

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
