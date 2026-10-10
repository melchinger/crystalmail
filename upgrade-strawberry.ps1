# Git's bundled perl shadows Strawberry Perl on PATH and breaks the
# vendored OpenSSL build (Locale::Maketext::Simple). This wrapper puts
# the Strawberry installation first, then defers to upgrade.ps1.
$StrawberryPerlBin = "D:\Programme\Strawberry\perl\bin"
if (-not (Test-Path -LiteralPath (Join-Path $StrawberryPerlBin "perl.exe"))) {
    throw "Strawberry Perl nicht unter '$StrawberryPerlBin' gefunden. Installiere es (z.B. 'winget install StrawberryPerl.StrawberryPerl') oder passe den Pfad an."
}
$env:PATH = "$StrawberryPerlBin;" + $env:PATH
Set-Location $PSScriptRoot
& .\upgrade.ps1 -Build
exit $LASTEXITCODE
