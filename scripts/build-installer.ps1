# Builds the Windows installer (NSIS) and the files for auto-update.
#
#   powershell -ExecutionPolicy Bypass -File scripts\build-installer.ps1 [-BaseUrl https://your.site/nanarchive]
#
# Output (in target\release\bundle\nsis):
#   Nanarchive_<version>_x64-setup.exe      the installer
#   Nanarchive_<version>_x64-setup.exe.sig  its update signature
#   latest.json                             upload next to the installer;
#                                           its address goes in Settings > Updates
#
# The signing key is %USERPROFILE%\.nanarchive\updater.key. It is not in the
# project. Keep a backup: without it, installed copies can't get updates.
param(
    [string]$BaseUrl = "https://example.com/nanarchive",
    [string]$Notes = ""
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$key = Join-Path $env:USERPROFILE ".nanarchive\updater.key"
if (-not (Test-Path $key)) { throw "Signing key not found: $key (create it with: npx tauri signer generate -w `"$key`")" }

$env:TAURI_SIGNING_PRIVATE_KEY = [IO.File]::ReadAllText($key).Trim()
if (-not $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD) { $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = "" }

Push-Location (Join-Path $root "app")
try {
    npx tauri build
    if ($LASTEXITCODE -ne 0) { throw "tauri build failed" }
} finally {
    Pop-Location
}

$conf = Get-Content (Join-Path $root "app\src-tauri\tauri.conf.json") -Raw | ConvertFrom-Json
$version = $conf.version
$dir = Join-Path $root "target\release\bundle\nsis"
$setup = Get-ChildItem $dir -Filter "*_${version}_*-setup.exe" | Select-Object -First 1
if (-not $setup) { throw "installer not found in $dir" }
$sig = [IO.File]::ReadAllText("$($setup.FullName).sig").Trim()

$latest = [ordered]@{
    version   = $version
    notes     = $Notes
    pub_date  = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    platforms = [ordered]@{
        "windows-x86_64" = [ordered]@{
            signature = $sig
            url       = "$($BaseUrl.TrimEnd('/'))/$([Uri]::EscapeDataString($setup.Name))"
        }
    }
}
$json = $latest | ConvertTo-Json -Depth 5
[IO.File]::WriteAllText((Join-Path $dir "latest.json"), $json, (New-Object Text.UTF8Encoding $false))
Write-Host "Installer: $($setup.FullName)"
Write-Host "Update file: $(Join-Path $dir 'latest.json')"
