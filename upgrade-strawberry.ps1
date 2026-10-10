$env:PATH = "D:\Programme\Strawberry\perl\bin;" + $env:PATH
Set-Location $PSScriptRoot
& .\upgrade.ps1 -Build
exit $LASTEXITCODE
