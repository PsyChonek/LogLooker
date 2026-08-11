<#
.SYNOPSIS
Submits LogLooker to the Windows Package Manager community repo.

.DESCRIPTION
Clones your fork of microsoft/winget-pkgs, copies the manifests from winget/
into manifests/p/PsyChonek/LogLooker/<version>/, commits and pushes a branch.
Opening the pull request is left to you - it needs a human to tick the checklist.

Run scripts/update-winget-manifests.ps1 first: the manifests it generates carry
the SHA256 and ProductCode of the MSIs actually released.

.PARAMETER Version
Version to submit, without the 'v' prefix. Defaults to the latest git tag.

.PARAMETER Fork
Your winget-pkgs fork, as owner/repo. Defaults to PsyChonek/winget-pkgs.

.PARAMETER GithubToken
Token with repo scope. Defaults to $env:GITHUB_TOKEN, then to gh's own token.

.PARAMETER DryRun
Print what would happen and change nothing.

.EXAMPLE
./scripts/submit-to-winget.ps1

.EXAMPLE
./scripts/submit-to-winget.ps1 -Version 1.2.0 -DryRun
#>
param(
    [Parameter(Mandatory = $false)]
    [string]$Version,

    [Parameter(Mandatory = $false)]
    [string]$Fork = "PsyChonek/winget-pkgs",

    [Parameter(Mandatory = $false)]
    [string]$GithubToken = $env:GITHUB_TOKEN,

    [switch]$DryRun
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $Root

if (-not $Version) {
    $Version = (git describe --tags --abbrev=0 2>$null) -replace '^v', ''
    if (-not $Version) {
        Write-Error "Could not determine version. Pass -Version, or create a tag first."
        exit 1
    }
}

# gh is already authenticated on this machine in the normal case, so prefer its
# token over asking for one to be exported
if (-not $GithubToken -and -not $DryRun) {
    try { $GithubToken = (gh auth token).Trim() } catch { $GithubToken = $null }
}
if (-not $GithubToken -and -not $DryRun) {
    Write-Error "No GitHub token. Run 'gh auth login', set GITHUB_TOKEN, or pass -GithubToken."
    exit 1
}

$manifestSource = Join-Path $Root "winget"
$expected = @(
    "PsyChonek.LogLooker.yaml",
    "PsyChonek.LogLooker.installer.yaml",
    "PsyChonek.LogLooker.locale.en-US.yaml"
)
foreach ($file in $expected) {
    if (-not (Test-Path (Join-Path $manifestSource $file))) {
        Write-Error "Missing $file - run ./scripts/update-winget-manifests.ps1 first."
        exit 1
    }
}

# A manifest for a different version than the one being submitted is the most
# likely mistake here, and winget-pkgs would reject it after the fact
$declared = (Select-String -Path (Join-Path $manifestSource $expected[0]) `
    -Pattern '^PackageVersion:\s*(.+)$').Matches.Groups[1].Value.Trim()
if ($declared -ne $Version) {
    Write-Error "winget/ declares version $declared but you are submitting $Version. Regenerate the manifests."
    exit 1
}

# So does a placeholder left in from a build that never happened
$installerText = Get-Content (Join-Path $manifestSource $expected[1]) -Raw
if ($installerText -match '0{64}' -or $installerText -match '\{0{8}-') {
    Write-Error "The installer manifest still has placeholder values - run ./scripts/update-winget-manifests.ps1 against the built MSIs."
    exit 1
}

Write-Host "Submitting LogLooker $Version to winget-pkgs via $Fork" -ForegroundColor Cyan

$branch = "loglooker-$Version"
$manifestDir = "manifests/p/PsyChonek/LogLooker/$Version"

if ($DryRun) {
    Write-Host "  [dry run] clone https://github.com/$Fork" -ForegroundColor Gray
    Write-Host "  [dry run] branch $branch" -ForegroundColor Gray
    Write-Host "  [dry run] copy winget/*.yaml -> $manifestDir" -ForegroundColor Gray
    Write-Host "  [dry run] commit and push" -ForegroundColor Gray
    exit 0
}

$temp = Join-Path $env:TEMP "winget-submit-loglooker-$Version"
if (Test-Path $temp) { Remove-Item $temp -Recurse -Force }
New-Item -ItemType Directory -Path $temp | Out-Null

try {
    # Shallow, single-branch: winget-pkgs is very large and none of its history
    # is needed to add three files
    $cloneUrl = "https://oauth2:$GithubToken@github.com/$Fork.git"
    Write-Host "Cloning $Fork..." -ForegroundColor Yellow
    git clone --depth 1 --single-branch $cloneUrl "$temp/winget-pkgs"
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Clone failed. Fork microsoft/winget-pkgs to $Fork first."
        exit 1
    }

    Push-Location "$temp/winget-pkgs"
    try {
        git checkout -b $branch
        New-Item -ItemType Directory -Path $manifestDir -Force | Out-Null
        Copy-Item -Path (Join-Path $manifestSource "*.yaml") -Destination $manifestDir -Force

        $copied = (Get-ChildItem $manifestDir -Filter "*.yaml").Count
        if ($copied -ne 3) {
            Write-Error "Expected 3 manifests in $manifestDir, found $copied"
            exit 1
        }
        Get-ChildItem $manifestDir | ForEach-Object { Write-Host "  + $manifestDir/$($_.Name)" }

        git add $manifestDir
        git -c user.name="PsyChonek" -c user.email="noreply@github.com" commit -m @"
Add LogLooker version $Version

Publisher: PsyChonek
Identifier: PsyChonek.LogLooker
Version: $Version

LogLooker fetches, caches and cross-searches log files from many services at
once, with substring or regex queries over one merged timeline and charts over
the whole result. Log sources, layouts, line parsing and search presets are
described by JSON plugins rather than built in.

Repository: https://github.com/PsyChonek/LogLooker
License: MIT
"@
        Write-Host "Pushing $branch..." -ForegroundColor Yellow
        git push -u origin $branch
        if ($LASTEXITCODE -ne 0) {
            Write-Error "Push failed"
            exit 1
        }

        Write-Host ""
        Write-Host "Pushed. Open the PR:" -ForegroundColor Green
        Write-Host "  https://github.com/microsoft/winget-pkgs/compare/master...$($Fork.Split('/')[0]):$branch"
        Write-Host "  Title: Add LogLooker version $Version"
    } finally {
        Pop-Location
    }
} finally {
    Remove-Item $temp -Recurse -Force -ErrorAction SilentlyContinue
}
