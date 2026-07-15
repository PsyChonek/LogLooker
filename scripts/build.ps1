# Build Tauri app MSI installer
# Usage: .\scripts\build.ps1 [-Bump major|minor|patch]
#   Bumps version, builds MSI, copies to dist/

param(
    [ValidateSet("major", "minor", "patch")]
    [string]$Bump = "patch"
)

$ErrorActionPreference = "Stop"
$Utf8NoBom = New-Object System.Text.UTF8Encoding $false
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $Root

# --- Read app name from tauri.conf.json ---
$tauriConf = Get-Content "src-tauri/tauri.conf.json" -Raw | ConvertFrom-Json
$AppName = $tauriConf.productName

# --- Read current version ---
$pkg = Get-Content "package.json" -Raw | ConvertFrom-Json
$Current = $pkg.version
Write-Host "Current version: $Current"

# --- Bump version ---
$parts = $Current.Split('.')
$Major = [int]$parts[0]
$Minor = [int]$parts[1]
$Patch = [int]$parts[2]

switch ($Bump) {
    "major" { $Major++; $Minor = 0; $Patch = 0 }
    "minor" { $Minor++; $Patch = 0 }
    "patch" { $Patch++ }
}
$Version = "$Major.$Minor.$Patch"
Write-Host "New version: $Version"

# --- Update version in all config files ---
# package.json
$pkgText = Get-Content "package.json" -Raw
$pkgText = $pkgText -replace '("version":\s*")([^"]+)"', "`${1}$Version`""
[System.IO.File]::WriteAllText((Resolve-Path "package.json"), $pkgText, $Utf8NoBom)

# tauri.conf.json
$confPath = "src-tauri/tauri.conf.json"
$confText = Get-Content $confPath -Raw
$confText = $confText -replace '("version":\s*")([^"]+)"', "`${1}$Version`""
[System.IO.File]::WriteAllText((Resolve-Path $confPath), $confText, $Utf8NoBom)

# Cargo.toml
$cargoPath = "src-tauri/Cargo.toml"
$cargo = Get-Content $cargoPath -Raw
$cargo = $cargo -replace '^version = ".*"', "version = `"$Version`""
[System.IO.File]::WriteAllText((Resolve-Path $cargoPath), $cargo)

Write-Host "Version bumped to $Version in package.json, tauri.conf.json, Cargo.toml"

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
