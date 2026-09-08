# Build Tauri app MSI installer
# Usage: .\scripts\build.ps1 [-Bump major|minor|patch]
#   Bumps version, builds MSI, copies to dist/

param(
    [ValidateSet("major", "minor", "patch")]
    [string]$Bump = "patch"
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $Root

# --- Read app name from tauri.conf.json ---
$tauriConf = Get-Content "src-tauri/tauri.conf.json" -Raw | ConvertFrom-Json
$AppName = $tauriConf.productName

# Use the same version update as GitHub Releases, including both lockfiles.
$Version = node scripts/bump-version.mjs $Bump
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Write-Host "Version bumped to $Version in package.json, tauri.conf.json, Cargo.toml and lockfiles"

# --- Build ---
Write-Host "Building Tauri app..."
npm run tauri build
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# --- Copy MSI to dist/ ---
$MsiSource = "src-tauri/target/release/bundle/msi/${AppName}_${Version}_x64_en-US.msi"
if (-not (Test-Path $MsiSource)) {
    Write-Error "ERROR: MSI not found at $MsiSource"
    exit 1
}

New-Item -ItemType Directory -Path "dist" -Force | Out-Null
$MsiDest = "dist/${AppName}-${Version}.msi"
Copy-Item $MsiSource $MsiDest
Write-Host "MSI copied to $MsiDest"
Write-Host "Done!"
